use crate::model::cfd::CfdOfferId;
use crate::model::Usd;
use crate::CfdOffer;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum TakerToMaker {
    TakeOffer { offer_id: CfdOfferId, quantity: Usd },
    // TODO: Currently the taker starts, can already send some stuff for signing over in the first message.
    StartContractSetup(CfdOfferId),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum MakerToTaker {
    CurrentOffer(Option<CfdOffer>),
    // TODO: Needs RejectOffer as well
    ConfirmTakeOffer(CfdOfferId),
    InvalidOfferId(CfdOfferId),
}
