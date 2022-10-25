use daemon::bdk::bitcoin::Amount;
use daemon::bdk::bitcoin::Network;
use daemon::bdk::bitcoin::Txid;
use daemon::bdk::BlockTime;
use daemon::identify;
use daemon::listen_protocols::does_maker_satisfy_taker_needs;
use daemon::listen_protocols::REQUIRED_MAKER_LISTEN_PROTOCOLS;
use daemon::online_status;
use daemon::projection::Cfd;
use model::Timestamp;
use rocket::response::stream::Event;
use serde::Serialize;
use std::collections::HashSet;

pub trait ToSseEvent {
    fn to_sse_event(&self) -> Event;
}

impl ToSseEvent for Vec<Cfd> {
    fn to_sse_event(&self) -> Event {
        Event::json(&self).event("cfds")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WalletInfo {
    #[serde(with = "daemon::bdk::bitcoin::util::amount::serde::as_btc")]
    balance: Amount,
    address: String,
    last_updated_at: Timestamp,
    transactions: Vec<TransactionDetails>,
    managed_wallet: bool,
}

#[derive(Serialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct TransactionDetails {
    pub txid: Txid,
    #[serde(with = "daemon::bdk::bitcoin::util::amount::serde::as_btc")]
    pub received: Amount,
    #[serde(with = "daemon::bdk::bitcoin::util::amount::serde::as_btc")]
    pub sent: Amount,
    pub confirmation_time: Option<BlockTime>,
    pub link: Option<String>,
}

impl From<(Network, &daemon::bdk::TransactionDetails)> for TransactionDetails {
    fn from((network, tx): (Network, &daemon::bdk::TransactionDetails)) -> Self {
        let txid = tx.txid;
        let link = match network {
            Network::Bitcoin => Some(format!("https://mempool.space/tx/{txid}")),
            Network::Testnet => Some(format!("https://mempool.space/testnet/tx/{txid}")),
            Network::Signet => Some(format!("https://mempool.space/signet/tx/{txid}")),
            Network::Regtest => None,
        };
        Self {
            txid,
            received: Amount::from_sat(tx.received),
            sent: Amount::from_sat(tx.sent),
            confirmation_time: tx.confirmation_time.clone(),
            link,
        }
    }
}

impl ToSseEvent for Option<model::WalletInfo> {
    fn to_sse_event(&self) -> Event {
        let wallet_info = self.as_ref().map(|wallet_info| {
            let transaction_details = wallet_info
                .transactions
                .iter()
                .map(|tx| (wallet_info.network, tx).into())
                .collect();

            WalletInfo {
                balance: wallet_info.balance,
                address: wallet_info.address.to_string(),
                last_updated_at: wallet_info.last_updated_at,
                transactions: transaction_details,
                managed_wallet: wallet_info.managed_wallet,
            }
        });

        Event::json(&wallet_info).event("wallet")
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct ConnectionStatus {
    online: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub enum ConnectionCloseReason {
    MakerVersionOutdated,
    TakerVersionOutdated,
}

impl ToSseEvent for online_status::ConnectionStatus {
    fn to_sse_event(&self) -> Event {
        let connected = match self {
            online_status::ConnectionStatus::Online => ConnectionStatus { online: true },
            online_status::ConnectionStatus::Offline => ConnectionStatus { online: false },
        };

        Event::json(&connected).event("maker_status")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct MakerCompatibility {
    /// Protocols that the maker version does not support, but the taker version requires
    unsupported_protocols: Option<HashSet<String>>,
}

impl MakerCompatibility {
    pub fn new(peer_info: &Option<identify::PeerInfo>) -> Self {
        let unsupported_protocols = peer_info.as_ref().map(|peer_info| {
            match does_maker_satisfy_taker_needs(
                &peer_info.protocols,
                REQUIRED_MAKER_LISTEN_PROTOCOLS,
            ) {
                Ok(_) => HashSet::new(),
                Err(missing_protocols) => missing_protocols,
            }
        });

        Self {
            unsupported_protocols,
        }
    }
}

impl ToSseEvent for Option<identify::PeerInfo> {
    fn to_sse_event(&self) -> Event {
        Event::json(&MakerCompatibility::new(self)).event("maker_compatibility")
    }
}
