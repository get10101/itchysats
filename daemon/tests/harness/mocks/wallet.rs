use crate::harness::cfd_protocol::dummy_wallet;
use anyhow::{anyhow, Result};
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{ecdsa, Amount, Txid};
use cfd_protocol::secp256k1_zkp::Secp256k1;
use cfd_protocol::{PartyParams, WalletExt};
use daemon::model::{Timestamp, WalletInfo};
use daemon::wallet;
use rand::thread_rng;
use xtra_productivity::xtra_productivity;

pub struct Wallet {
    party_params: bool,
    wallet_info: Option<WalletInfo>,
    psbt: Option<PartiallySignedTransaction>,
    txid: Option<Txid>,
}

impl Wallet {
    pub fn with_party_params(mut self) -> Self {
        self.party_params = true;
        self
    }

    pub fn with_wallet_info(mut self) -> Self {
        let s = Secp256k1::new();
        let public_key = ecdsa::PublicKey::new(s.generate_keypair(&mut thread_rng()).1);
        let address = bdk::bitcoin::Address::p2pkh(&public_key, bdk::bitcoin::Network::Testnet);

        self.wallet_info = Some(WalletInfo {
            balance: bdk::bitcoin::Amount::ONE_BTC,
            address,
            last_updated_at: Timestamp::now().unwrap(),
        });

        self
    }

    pub fn with_psbt(mut self, psbt: PartiallySignedTransaction) -> Self {
        self.psbt = Some(psbt);
        self
    }

    pub fn with_txid(mut self, txid: Txid) -> Self {
        self.txid = Some(txid);
        self
    }
}

impl xtra::Actor for Wallet {}

#[xtra_productivity(message_impl = false)]
impl Wallet {
    async fn handle(&mut self, msg: wallet::BuildPartyParams) -> Result<PartyParams> {
        if self.party_params {
            let mut rng = thread_rng();
            let wallet = dummy_wallet(&mut rng, Amount::from_btc(0.4).unwrap(), 5).unwrap();

            let party_params = wallet
                .build_party_params(msg.amount, msg.identity_pk)
                .unwrap();

            return Ok(party_params);
        }

        panic!("Stub not configured to return party params")
    }
    async fn handle(&mut self, _msg: wallet::Sync) -> Result<WalletInfo> {
        self.wallet_info
            .clone()
            .ok_or_else(|| anyhow!("Stub not configured to return WalletInfo"))
    }
    async fn handle(&mut self, _msg: wallet::Sign) -> Result<PartiallySignedTransaction> {
        self.psbt
            .clone()
            .ok_or_else(|| anyhow!("Stub not configured to return PartiallySignedTransaction"))
    }
    async fn handle(&mut self, _msg: wallet::TryBroadcastTransaction) -> Result<Txid> {
        self.txid
            .ok_or_else(|| anyhow!("Stub not configured to return Txid"))
    }
}

impl Default for Wallet {
    fn default() -> Self {
        Wallet {
            party_params: false,
            wallet_info: None,
            psbt: None,
            txid: None,
        }
    }
}
