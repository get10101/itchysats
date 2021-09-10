use crate::model::cfd::{Cfd, CfdOffer};
use bdk::bitcoin::Amount;
use rocket::response::stream::Event;

pub trait ToSseEvent {
    fn to_sse_event(&self) -> Event;
}

impl ToSseEvent for Vec<Cfd> {
    fn to_sse_event(&self) -> Event {
        Event::json(self).event("cfds")
    }
}

impl ToSseEvent for Option<CfdOffer> {
    fn to_sse_event(&self) -> Event {
        Event::json(self).event("offer")
    }
}

impl ToSseEvent for Amount {
    fn to_sse_event(&self) -> Event {
        Event::json(&self.as_btc()).event("balance")
    }
}
