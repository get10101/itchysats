use crate::model::cfd::OrderId;
use crate::model::Usd;
use crate::Order;
use bdk::bitcoin::secp256k1::Signature;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Address, Amount, PublicKey};
use cfd_protocol::secp256k1_zkp::EcdsaAdaptorSignature;
use cfd_protocol::{CfdTransactions, PartyParams, PunishParams};
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum TakerToMaker {
    TakeOrder { order_id: OrderId, quantity: Usd },
    // TODO: Currently the taker starts, can already send some stuff for signing over in the first
    // message.
    StartContractSetup(OrderId),
    Protocol(SetupMsg),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
#[allow(clippy::large_enum_variant)]
pub enum MakerToTaker {
    CurrentOrder(Option<Order>),
    // TODO: Needs RejectOrder as well
    ConfirmTakeOrder(OrderId), // TODO: Include payout curve in "accept" message from maker
    InvalidOrderId(OrderId),
    Protocol(SetupMsg),
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

    pub fn try_into_msg2(self) -> Result<Msg2, Self> {
        if let Self::Msg2(v) = self {
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
    pub commit: EcdsaAdaptorSignature,
    pub cets: Vec<(RangeInclusive<u64>, EcdsaAdaptorSignature)>,
    pub refund: Signature,
}

impl From<CfdTransactions> for Msg1 {
    fn from(txs: CfdTransactions) -> Self {
        Self {
            commit: txs.commit.1,
            cets: txs
                .cets
                .into_iter()
                .map(|(_, sig, digits)| (digits.range(), sig))
                .collect(),
            refund: txs.refund.1,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Msg2 {
    pub signed_lock: PartiallySignedTransaction, // TODO: Use binary representation
}
