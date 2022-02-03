use crate::connection;
use crate::to_sse_event::ConnectionCloseReason::MakerVersionOutdated;
use crate::to_sse_event::ConnectionCloseReason::TakerVersionOutdated;
use rocket::response::stream::Event;
use serde::Serialize;
use shared_bin::ToSseEvent;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectionStatus {
    online: bool,
    connection_close_reason: Option<ConnectionCloseReason>,
}

#[derive(Debug, Clone, Serialize)]
pub enum ConnectionCloseReason {
    MakerVersionOutdated,
    TakerVersionOutdated,
}

impl ToSseEvent for connection::ConnectionStatus {
    fn to_sse_event(&self) -> Event {
        let connected = match self {
            connection::ConnectionStatus::Online => ConnectionStatus {
                online: true,
                connection_close_reason: None,
            },
            connection::ConnectionStatus::Offline { reason } => ConnectionStatus {
                online: false,
                connection_close_reason: reason.as_ref().map(|g| match g {
                    connection::ConnectionCloseReason::VersionMismatch {
                        maker_version,
                        taker_version,
                    } => {
                        if *maker_version < *taker_version {
                            MakerVersionOutdated
                        } else {
                            TakerVersionOutdated
                        }
                    }
                }),
            },
        };

        Event::json(&connected).event("maker_status")
    }
}
