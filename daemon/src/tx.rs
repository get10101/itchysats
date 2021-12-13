use bdk::bitcoin::Network;
use bdk::bitcoin::Txid;
use serde::Serialize;

use crate::model::cfd;

#[derive(Debug, Clone, Serialize)]
pub struct TxUrl {
    pub label: TxLabel,
    pub url: String,
}

/// Construct a mempool.space URL for a given txid
pub fn to_mempool_url(txid: Txid, network: Network) -> String {
    match network {
        Network::Bitcoin => format!("https://mempool.space/tx/{}", txid),
        Network::Testnet => format!("https://mempool.space/testnet/tx/{}", txid),
        Network::Signet => format!("https://mempool.space/signet/tx/{}", txid),
        Network::Regtest => txid.to_string(),
    }
}

impl TxUrl {
    pub fn new(txid: Txid, network: Network, label: TxLabel) -> Self {
        Self {
            label,
            url: to_mempool_url(txid, network),
        }
    }
}

pub fn to_tx_url_list(state: cfd::CfdState, network: Network) -> Vec<TxUrl> {
    use cfd::CfdState::*;

    let tx_ub = TxUrlBuilder::new(network);

    match state {
        PendingOpen { dlc, .. } => {
            vec![tx_ub.lock(&dlc)]
        }
        PendingCommit { dlc, .. } => vec![tx_ub.lock(&dlc), tx_ub.commit(&dlc)],
        OpenCommitted { dlc, .. } => vec![tx_ub.lock(&dlc), tx_ub.commit(&dlc)],
        Open {
            dlc,
            collaborative_close,
            ..
        } => {
            let mut tx_urls = vec![tx_ub.lock(&dlc)];
            if let Some(collaborative_close) = collaborative_close {
                tx_urls.push(tx_ub.collaborative_close(collaborative_close.tx.txid()));
            }
            tx_urls
        }
        PendingCet {
            dlc, attestation, ..
        } => vec![
            tx_ub.lock(&dlc),
            tx_ub.commit(&dlc),
            tx_ub.cet(attestation.txid()),
        ],
        Closed {
            payout: cfd::Payout::Cet(attestation),
            ..
        } => vec![tx_ub.cet(attestation.txid())],
        Closed {
            payout: cfd::Payout::CollaborativeClose(collaborative_close),
            ..
        } => {
            vec![tx_ub.collaborative_close(collaborative_close.tx.txid())]
        }
        PendingRefund { dlc, .. } => vec![tx_ub.lock(&dlc), tx_ub.commit(&dlc), tx_ub.refund(&dlc)],
        Refunded { dlc, .. } => vec![tx_ub.refund(&dlc)],
        OutgoingOrderRequest { .. }
        | IncomingOrderRequest { .. }
        | Accepted { .. }
        | Rejected { .. }
        | ContractSetup { .. }
        | SetupFailed { .. } => vec![],
    }
}

struct TxUrlBuilder {
    network: Network,
}

impl TxUrlBuilder {
    pub fn new(network: Network) -> Self {
        Self { network }
    }

    pub fn lock(&self, dlc: &cfd::Dlc) -> TxUrl {
        TxUrl::new(dlc.lock.0.txid(), self.network, TxLabel::Lock)
    }

    pub fn commit(&self, dlc: &cfd::Dlc) -> TxUrl {
        TxUrl::new(dlc.commit.0.txid(), self.network, TxLabel::Commit)
    }

    pub fn cet(&self, txid: Txid) -> TxUrl {
        TxUrl::new(txid, self.network, TxLabel::Cet)
    }

    pub fn collaborative_close(&self, txid: Txid) -> TxUrl {
        TxUrl::new(txid, self.network, TxLabel::Collaborative)
    }

    pub fn refund(&self, dlc: &cfd::Dlc) -> TxUrl {
        TxUrl::new(dlc.refund.0.txid(), self.network, TxLabel::Refund)
    }
}

#[derive(Debug, Clone, Serialize)]
pub enum TxLabel {
    Lock,
    Commit,
    Cet,
    Refund,
    Collaborative,
}
