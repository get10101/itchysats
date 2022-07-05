use anyhow::Result;
use async_trait::async_trait;
use bdk_ext::new_test_wallet;
use daemon::bdk;
use daemon::bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use daemon::bdk::bitcoin::Amount;
use daemon::bdk::bitcoin::Txid;
use daemon::bdk::wallet::tx_builder::TxOrdering;
use daemon::bdk::wallet::AddressIndex;
use daemon::bdk::FeeRate;
use daemon::maia_core::secp256k1_zkp::Secp256k1;
use daemon::maia_core::PartyParams;
use daemon::maia_core::TxBuilderExt;
use daemon::wallet;
use mockall::*;
use model::Timestamp;
use model::WalletInfo;
use rand::thread_rng;
use std::sync::Arc;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

/// Test Stub simulating the Wallet actor.
/// Serves as an entrypoint for injected mock handlers.
pub struct WalletActor {
    mock: Arc<Mutex<MockWallet>>,
}

impl WalletActor {
    pub fn new() -> (WalletActor, Arc<Mutex<MockWallet>>) {
        let mock = Arc::new(Mutex::new(MockWallet::new()));
        let actor = Self { mock: mock.clone() };

        (actor, mock)
    }
}

#[async_trait]
impl xtra::Actor for WalletActor {
    type Stop = ();

    async fn stopped(self) -> Self::Stop {}
}

#[xtra_productivity]
impl WalletActor {
    async fn handle(&mut self, msg: wallet::BuildPartyParams) -> Result<PartyParams> {
        self.mock.lock().await.build_party_params(msg)
    }
    async fn handle(&mut self, msg: wallet::Sign) -> Result<PartiallySignedTransaction> {
        self.mock.lock().await.sign(msg)
    }
    async fn handle(&mut self, msg: wallet::Withdraw) -> Result<Txid> {
        self.mock.lock().await.withdraw(msg)
    }
    async fn handle(&mut self, msg: wallet::Sync) {
        self.mock.lock().await.sync(msg)
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

    fn withdraw(&mut self, _msg: wallet::Withdraw) -> Result<Txid> {
        unreachable!("mockall will reimplement this method")
    }

    fn sync(&mut self, _msg: wallet::Sync) {
        unreachable!("mockall will reimplement this method")
    }
}

#[allow(dead_code)]
/// tells the user they have plenty of moneys
fn dummy_wallet_info() -> Result<WalletInfo> {
    let s = Secp256k1::new();
    let public_key = bdk::bitcoin::PublicKey::new(s.generate_keypair(&mut thread_rng()).1);
    let address = bdk::bitcoin::Address::p2pkh(&public_key, bdk::bitcoin::Network::Testnet);

    Ok(WalletInfo {
        balance: bdk::bitcoin::Amount::ONE_BTC,
        address,
        last_updated_at: Timestamp::now(),
    })
}

pub fn build_party_params(msg: wallet::BuildPartyParams) -> Result<PartyParams> {
    let mut rng = thread_rng();
    let wallet = new_test_wallet(&mut rng, Amount::from_btc(0.4).unwrap(), 5).unwrap();

    let mut builder = wallet.build_tx();

    builder
        .ordering(TxOrdering::Bip69Lexicographic) // TODO: I think this is pointless but we did this in maia.
        .fee_rate(FeeRate::from_sat_per_vb(1.0))
        .add_2of2_multisig_recipient(msg.amount);

    let (psbt, _) = builder.finish()?;

    Ok(PartyParams {
        lock_psbt: psbt,
        identity_pk: msg.identity_pk,
        lock_amount: msg.amount,
        address: wallet.get_address(AddressIndex::New)?.address,
    })
}
