use crate::harness::maia::dummy_wallet;
use anyhow::Result;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{ecdsa, Amount, Txid};
use daemon::model::{Timestamp, WalletInfo};
use daemon::wallet::{self};
use maia::secp256k1_zkp::Secp256k1;
use maia::{PartyParams, WalletExt};
use mockall::*;
use rand::thread_rng;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Wallet actor.
/// Serves as an entrypoint for injected mock handlers.
pub struct WalletActor {
    pub mock: Arc<Mutex<dyn Wallet + Send>>,
}

impl xtra::Actor for WalletActor {}

#[xtra_productivity(message_impl = false)]
impl WalletActor {
    async fn handle(&mut self, msg: wallet::BuildPartyParams) -> Result<PartyParams> {
        self.mock.lock().await.build_party_params(msg)
    }
    async fn handle(&mut self, msg: wallet::Sync) -> Result<WalletInfo> {
        tracing::info!("We are handling the wallet sync msg");
        self.mock.lock().await.sync(msg)
    }
    async fn handle(&mut self, msg: wallet::Sign) -> Result<PartiallySignedTransaction> {
        self.mock.lock().await.sign(msg)
    }
    async fn handle(&mut self, msg: wallet::TryBroadcastTransaction) -> Result<Txid> {
        self.mock.lock().await.broadcast(msg)
    }
}

#[automock]
pub trait Wallet {
    fn build_party_params(&mut self, _msg: wallet::BuildPartyParams) -> Result<PartyParams> {
        unreachable!("mockall will reimplement this method")
    }

    fn sign(&mut self, _msg: wallet::Sign) -> Result<PartiallySignedTransaction> {
        unreachable!("mockall will reimplement this method")
    }

    fn broadcast(&mut self, _msg: wallet::TryBroadcastTransaction) -> Result<Txid> {
        unreachable!("mockall will reimplement this method")
    }

    fn sync(&mut self, _msg: wallet::Sync) -> Result<WalletInfo> {
        unreachable!("mockall will reimplement this method")
    }
}

#[allow(dead_code)]
/// tells the user they have plenty of moneys
fn dummy_wallet_info() -> Result<WalletInfo> {
    let s = Secp256k1::new();
    let public_key = ecdsa::PublicKey::new(s.generate_keypair(&mut thread_rng()).1);
    let address = bdk::bitcoin::Address::p2pkh(&public_key, bdk::bitcoin::Network::Testnet);

    Ok(WalletInfo {
        balance: bdk::bitcoin::Amount::ONE_BTC,
        address,
        last_updated_at: Timestamp::now()?,
    })
}

pub fn build_party_params(msg: wallet::BuildPartyParams) -> Result<PartyParams> {
    let mut rng = thread_rng();
    let wallet = dummy_wallet(&mut rng, Amount::from_btc(0.4).unwrap(), 5).unwrap();

    let party_params = wallet
        .build_party_params(msg.amount, msg.identity_pk)
        .unwrap();
    Ok(party_params)
}
