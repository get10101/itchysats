use anyhow::bail;
use anyhow::Result;
use bdk::bitcoin::psbt::PartiallySignedTransaction;
use bdk::bitcoin::secp256k1::ecdsa::Signature;
use bdk::bitcoin::Address;
use bdk::bitcoin::Amount;
use bdk::bitcoin::PublicKey;
use maia_core::secp256k1_zkp::EcdsaAdaptorSignature;
use maia_core::CfdTransactions;
use maia_core::PartyParams;
use maia_core::PunishParams;
use model::Contracts;
use model::Leverage;
use model::Offer;
use model::OrderId;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::ops::RangeInclusive;

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum TakerMessage {
    PlaceOrder {
        id: OrderId,
        offer: Offer,
        quantity: Contracts,
        leverage: Leverage,
    },
    ContractSetupMsg(Box<SetupMsg>),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum MakerMessage {
    Decision(Decision),
    ContractSetupMsg(Box<SetupMsg>),
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) enum Decision {
    Accept,
    Reject,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum SetupMsg {
    /// Message enabling setting up lock and based on that commit, refund and cets
    ///
    /// Each party sends and receives this message.
    /// After receiving this message each party is able to construct the lock transaction.
    Msg0(Msg0),
    /// Message that ensures complete commit, cets and refund transactions
    ///
    /// Each party sends and receives this message.
    /// After receiving this message the commit, refund and cet transactions are complete.
    /// Once verified we can sign and send the lock PSBT.
    Msg1(Msg1),
    /// Message adding signature to the lock PSBT
    ///
    /// Each party sends and receives this message.
    /// Upon receiving this message from the other party we merge our signature and then the lock
    /// tx is fully signed and can be published on chain.
    Msg2(Msg2),
    /// Message acknowledging that we received everything
    ///
    /// Simple ACK message used at the end of the message exchange to ensure that both parties sent
    /// and received everything and we did not run into timeouts on the other side.
    /// This is used to avoid one party publishing the lock transaction while the other party ran
    /// into a timeout.
    Msg3(Msg3),
}

#[derive(Serialize, Deserialize, Debug)]
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

#[derive(Serialize, Deserialize, Debug)]
pub struct Msg1 {
    pub commit: EcdsaAdaptorSignature,
    pub cets: HashMap<String, Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>>,
    pub refund: Signature,
}

impl From<CfdTransactions> for Msg1 {
    fn from(txs: CfdTransactions) -> Self {
        let cets = txs
            .cets
            .into_iter()
            .map(|grouped_cets| {
                (
                    grouped_cets.event.id,
                    grouped_cets
                        .cets
                        .into_iter()
                        .map(|(_, encsig, digits)| (digits.range(), encsig))
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<HashMap<_, _>>();
        Self {
            commit: txs.commit.1,
            cets,
            refund: txs.refund.1,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Msg2 {
    pub signed_lock: PartiallySignedTransaction, // TODO: Use binary representation
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug)]
pub struct Msg3;

impl SetupMsg {
    pub fn try_into_msg0(self) -> Result<Msg0> {
        if let Self::Msg0(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg0")
        }
    }

    pub fn try_into_msg1(self) -> Result<Msg1> {
        if let Self::Msg1(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg1")
        }
    }

    pub fn try_into_msg2(self) -> Result<Msg2> {
        if let Self::Msg2(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg2")
        }
    }

    pub fn try_into_msg3(self) -> Result<Msg3> {
        if let Self::Msg3(v) = self {
            Ok(v)
        } else {
            bail!("Not Msg3")
        }
    }
}

impl TryFrom<MakerMessage> for SetupMsg {
    type Error = anyhow::Error;

    fn try_from(value: MakerMessage) -> Result<Self> {
        match value {
            MakerMessage::Decision(_) => bail!("Expected SetupMsg, got decision"),
            MakerMessage::ContractSetupMsg(msg) => Ok(*msg),
        }
    }
}

impl TryFrom<TakerMessage> for SetupMsg {
    type Error = anyhow::Error;

    fn try_from(value: TakerMessage) -> Result<Self> {
        match value {
            TakerMessage::PlaceOrder { .. } => bail!("Expected SetupMsg, got order placement"),
            TakerMessage::ContractSetupMsg(msg) => Ok(*msg),
        }
    }
}
