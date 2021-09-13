use crate::model::cfd::CfdOfferId;
use crate::model::Usd;
use crate::CfdOffer;
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Address, Amount, PublicKey, Txid};
use cfd_protocol::{CfdTransactions, PartyParams, PunishParams};
use serde::{Deserialize, Serialize};
use serde_with::{serde_as, DisplayFromStr};

#[serde_as]
#[derive(Debug, Serialize, Deserialize)]
pub struct AdaptorSignature(#[serde_as(as = "DisplayFromStr")] cfd_protocol::EcdsaAdaptorSignature);

impl std::ops::Deref for AdaptorSignature {
    type Target = cfd_protocol::EcdsaAdaptorSignature;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum TakerToMaker {
    TakeOffer { offer_id: CfdOfferId, quantity: Usd },
    // TODO: Currently the taker starts, can already send some stuff for signing over in the first
    // message.
    StartContractSetup(CfdOfferId),
    Protocol(SetupMsg),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum MakerToTaker {
    CurrentOffer(Option<CfdOffer>),
    // TODO: Needs RejectOffer as well
    ConfirmTakeOffer(CfdOfferId), // TODO: Include payout curve in "accept" message from maker
    InvalidOfferId(CfdOfferId),
    Protocol(SetupMsg),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SetupMsg {
    Msg0(Msg0),
    Msg1(Msg1),
}

impl SetupMsg {
    pub fn try_into_msg0(self) -> Result<Msg0, Self> {
        if let Self::Msg0(v) = self {
            Ok(v)
        } else {
            Err(self)
        }
    }

    pub fn try_into_msg1(self) -> Result<Msg1, Self> {
        if let Self::Msg1(v) = self {
            Ok(v)
        } else {
            Err(self)
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Msg0 {
    pub lock_psbt: PartiallySignedTransaction, // TODO: Use binary representation
    pub identity_pk: PublicKey,
    #[serde(with = "bdk::bitcoin::util::amount::serde::as_sat")]
    pub lock_amount: Amount,
    pub address: Address,
    pub revocation_pk: PublicKey,
    pub publish_pk: PublicKey,
}

impl From<(PartyParams, PunishParams)> for Msg0 {
    fn from((party, punish): (PartyParams, PunishParams)) -> Self {
        let PartyParams {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
        } = party;
        let PunishParams {
            revocation_pk,
            publish_pk,
        } = punish;

        Self {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
            revocation_pk,
            publish_pk,
        }
    }
}

impl From<Msg0> for (PartyParams, PunishParams) {
    fn from(msg0: Msg0) -> Self {
        let Msg0 {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
            revocation_pk,
            publish_pk,
        } = msg0;

        let party = PartyParams {
            lock_psbt,
            identity_pk,
            lock_amount,
            address,
        };
        let punish = PunishParams {
            revocation_pk,
            publish_pk,
        };

        (party, punish)
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Msg1 {
    pub commit: AdaptorSignature,
    pub cets: Vec<(Txid, AdaptorSignature)>,
    pub refund: Signature,
}

impl From<CfdTransactions> for Msg1 {
    fn from(txs: CfdTransactions) -> Self {
        Self {
            commit: AdaptorSignature(txs.commit.1),
            cets: txs
                .cets
                .into_iter()
                .map(|(tx, sig, _, _)| (tx.txid(), AdaptorSignature(sig)))
                .collect(),
            refund: txs.refund.1,
        }
    }
}
