use crate::olivia::BitMexPriceEventId;
use crate::CompleteFee;
use crate::Dlc;
use crate::FeeAccount;
use crate::FundingFee;
use crate::Leverage;
use crate::Price;
use crate::RevokedCommit;
use crate::TxFeeRate;
use crate::Txid;
use crate::Usd;
use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use bdk::bitcoin::secp256k1::SecretKey;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Script;
use bdk::miniscript::DescriptorTrait;
use bdk_ext::SecretKeyExt;
use maia_core::secp256k1_zkp;
use maia_core::secp256k1_zkp::EcdsaAdaptorSignature;
use maia_core::secp256k1_zkp::SECP256K1;

#[derive(Debug, Clone, Copy)]
pub enum Version {
    /// Version one of the rollover protocol
    ///
    /// This version cannot handle charging for "missed" rollovers yet, i.e. the hours to charge is
    /// always set to 1 hour. This version is needed for clients that are <= daemon version
    /// `0.4.7`.
    V1,
    /// Version two of the rollover protocol
    ///
    /// This version can handle charging for "missed" rollovers, i.e. we calculate the hours to
    /// charge based on the oracle event timestamp of the last successful rollover.
    V2,
    /// Version two of the rollover protocol
    ///
    /// This version calculates the time to extend the settlement time
    /// by using the `BitMexPriceEventId` of the settlement event
    /// associated with the rollover.
    V3,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match *self {
            Version::V1 => write!(f, "V1"),
            Version::V2 => write!(f, "V2"),
            Version::V3 => write!(f, "V3"),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RolloverParams {
    pub price: Price,
    pub quantity: Usd,
    pub long_leverage: Leverage,
    pub short_leverage: Leverage,
    pub refund_timelock: u32,
    pub fee_rate: TxFeeRate,
    pub fee_account: FeeAccount,
    pub current_fee: FundingFee,
    pub version: Version,
}

impl RolloverParams {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        price: Price,
        quantity: Usd,
        long_leverage: Leverage,
        short_leverage: Leverage,
        refund_timelock: u32,
        fee_rate: TxFeeRate,
        fee_account: FeeAccount,
        current_fee: FundingFee,
        version: Version,
    ) -> Self {
        Self {
            price,
            quantity,
            long_leverage,
            short_leverage,
            refund_timelock,
            fee_rate,
            fee_account,
            current_fee,
            version,
        }
    }

    pub fn funding_fee(&self) -> &FundingFee {
        &self.current_fee
    }

    pub fn complete_fee_before_rollover(&self) -> CompleteFee {
        self.fee_account.settle()
    }
}

/// Parameters associated with the base DLC involved in a rollover.
///
/// The base DLC is the DLC from which both parties start a rollover.
/// Usually this is the latest DLC for both parties, but because the
/// taker can fall behind in very specific situations, the maker may
/// have to choose a DLC other than the latest one.
pub struct BaseDlcParams {
    pub base_commit_params: BaseCommitParams,
    /// The revoked commit transaction information for all the DLCs
    /// prior to the base one.
    pub revoked_commits: Vec<RevokedCommit>,
}

/// The commit transaction information about the base DLC.
///
/// This information will be used to generate the next `RevokedCommit`
/// after we roll over, in combination with the counterparty's
/// revocation sk.
pub struct BaseCommitParams {
    // To punish.
    pub commit_encsig_ours: EcdsaAdaptorSignature,
    pub revocation_pk_theirs: PublicKey,
    pub publish_pk_theirs: PublicKey,

    // To monitor.
    pub commit_txid: Txid,
    pub commit_script_pubkey: Script,

    // To allow rolling over from arbitrary base.
    pub settlement_event_id: BitMexPriceEventId,
    pub revocation_sk_ours: SecretKey,
    pub complete_fee: CompleteFee,
}

impl Dlc {
    pub fn base_dlc_params(
        &self,
        base_commit_txid: Txid,
        complete_fee: CompleteFee,
    ) -> Result<BaseDlcParams> {
        Ok(if self.commit.0.txid() == base_commit_txid {
            self.base_dlc_params_from_latest(complete_fee)
        } else {
            let params = self.base_dlc_params_from_revoked_commit(base_commit_txid)?;
            tracing::info!(commit_txid=%base_commit_txid, "Building base DLC params for rollover from previous commit TX");

            params
        })
    }

    /// Construct the `BaseDlcParams` based on the latest DLC.
    ///
    /// This is called if neither party has fallen behind in terms of
    /// rollovers.
    pub fn base_dlc_params_from_latest(&self, complete_fee: CompleteFee) -> BaseDlcParams {
        BaseDlcParams {
            base_commit_params: BaseCommitParams {
                commit_encsig_ours: self.commit.1,
                revocation_pk_theirs: self.revocation_pk_counterparty,
                publish_pk_theirs: self.publish_pk_counterparty,
                commit_txid: self.commit.0.txid(),
                commit_script_pubkey: self.commit.2.script_pubkey(),
                settlement_event_id: self.settlement_event_id,
                revocation_sk_ours: self.revocation,
                complete_fee,
            },
            revoked_commits: self.revoked_commit.clone(),
        }
    }

    /// Construct the `BaseDlcParams` based on a revoked DLC. The
    /// commit TXID is used to identify the rollover from which both
    /// parties intend to start the new rollover.
    ///
    /// This is called if a party is behind in terms of rollovers.
    fn base_dlc_params_from_revoked_commit(
        &self,
        revoked_commit_txid: Txid,
    ) -> Result<BaseDlcParams> {
        let RevokedCommit {
            encsig_ours: commit_encsig_ours,
            revocation_sk_ours,
            revocation_sk_theirs,
            publication_pk_theirs: publish_pk_theirs,
            txid: commit_txid,
            script_pubkey: commit_script_pubkey,
            settlement_event_id,
            complete_fee,
        } = self
            .revoked_commit
            .iter()
            .find(|revoked_commit| revoked_commit.txid == revoked_commit_txid)
            .with_context(|| format!("Unknown commit TXID {revoked_commit_txid}"))?
            .clone();

        let settlement_event_id = settlement_event_id.context("Missing settlement event id")?;
        let complete_fee = complete_fee.context("Missing complete fee")?;
        let revocation_sk_ours = revocation_sk_ours.context("Missing own revocation sk")?;

        let revoked_commits = self
            .revoked_commit
            .iter()
            .take_while(|commit| commit.txid != revoked_commit_txid)
            .cloned()
            .collect();

        Ok(BaseDlcParams {
            base_commit_params: BaseCommitParams {
                commit_encsig_ours,
                revocation_sk_ours,
                revocation_pk_theirs: PublicKey::new(revocation_sk_theirs.to_public_key()),
                publish_pk_theirs,
                commit_txid,
                commit_script_pubkey,
                settlement_event_id,
                complete_fee,
            },
            revoked_commits,
        })
    }
}

impl BaseDlcParams {
    /// Produce the set of revoked commit transactions that correspond
    /// to rolling over from this DLC as a base.
    ///
    /// The success of this operation hinges on the revocation secret
    /// key provided by the counterparty. If must match the revocation
    /// public key they provided when building the base DLC.
    pub fn revoke_base_commit_tx(
        self,
        revocation_sk_theirs: SecretKey,
    ) -> Result<Vec<RevokedCommit>> {
        let derived_revocation_pk_theirs = PublicKey::new(
            secp256k1_zkp::PublicKey::from_secret_key(SECP256K1, &revocation_sk_theirs),
        );

        ensure!(derived_revocation_pk_theirs == self.base_commit_params.revocation_pk_theirs);

        let new_revoked_commit = RevokedCommit {
            encsig_ours: self.base_commit_params.commit_encsig_ours,
            revocation_sk_ours: Some(self.base_commit_params.revocation_sk_ours),
            revocation_sk_theirs,
            publication_pk_theirs: self.base_commit_params.publish_pk_theirs,
            txid: self.base_commit_params.commit_txid,
            script_pubkey: self.base_commit_params.commit_script_pubkey,
            settlement_event_id: Some(self.base_commit_params.settlement_event_id),
            complete_fee: Some(self.base_commit_params.complete_fee),
        };

        let mut revoked_commits = self.revoked_commits;
        revoked_commits.push(new_revoked_commit);

        Ok(revoked_commits)
    }

    pub fn revocation_sk_ours(&self) -> SecretKey {
        self.base_commit_params.revocation_sk_ours
    }

    pub fn settlement_event_id(&self) -> BitMexPriceEventId {
        self.base_commit_params.settlement_event_id
    }

    pub fn complete_fee(&self) -> CompleteFee {
        self.base_commit_params.complete_fee
    }
}
