use crate::model::WalletInfo;
use anyhow::{anyhow, bail, Context, Result};
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Amount, PublicKey, Transaction, Txid};
use bdk::blockchain::{ElectrumBlockchain, NoopProgress};
use bdk::wallet::AddressIndex;
use bdk::{electrum_client, Error, KeychainKind, SignOptions};
use cfd_protocol::{PartyParams, WalletExt};
use rocket::serde::json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;

const SLED_TREE_NAME: &str = "wallet";

#[derive(Clone)]
pub struct Wallet {
    wallet: Arc<Mutex<bdk::Wallet<ElectrumBlockchain, bdk::sled::Tree>>>,
}

#[derive(thiserror::Error, Debug, Clone, Copy)]
#[error("The transaction is already in the blockchain")]
pub struct TransactionAlreadyInBlockchain;

impl Wallet {
    pub async fn new(
        electrum_rpc_url: &str,
        wallet_dir: &Path,
        ext_priv_key: ExtendedPrivKey,
    ) -> Result<Self> {
        let client = bdk::electrum_client::Client::new(electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        // TODO: Replace with sqlite once https://github.com/bitcoindevkit/bdk/pull/376 is merged.
        let db = bdk::sled::open(wallet_dir)?.open_tree(SLED_TREE_NAME)?;

        let wallet = bdk::Wallet::new(
            bdk::template::Bip84(ext_priv_key, KeychainKind::External),
            Some(bdk::template::Bip84(ext_priv_key, KeychainKind::Internal)),
            ext_priv_key.network,
            db,
            ElectrumBlockchain::from(client),
        )?;

        let wallet = Arc::new(Mutex::new(wallet));

        Ok(Self { wallet })
    }

    pub async fn build_party_params(
        &self,
        amount: Amount,
        identity_pk: PublicKey,
    ) -> Result<PartyParams> {
        let wallet = self.wallet.lock().await;
        wallet.build_party_params(amount, identity_pk)
    }

    pub async fn sync(&self) -> Result<WalletInfo> {
        let wallet = self.wallet.lock().await;
        wallet.sync(NoopProgress, None)?;

        let balance = wallet.get_balance()?;

        let address = wallet.get_address(AddressIndex::LastUnused)?.address;

        let wallet_info = WalletInfo {
            balance: Amount::from_sat(balance),
            address,
            last_updated_at: SystemTime::now(),
        };

        Ok(wallet_info)
    }

    pub async fn sign(
        &self,
        mut psbt: PartiallySignedTransaction,
    ) -> Result<PartiallySignedTransaction> {
        let wallet = self.wallet.lock().await;

        wallet
            .sign(
                &mut psbt,
                SignOptions {
                    trust_witness_utxo: true,
                    ..Default::default()
                },
            )
            .context("could not sign transaction")?;

        Ok(psbt)
    }

    pub async fn try_broadcast_transaction(&self, tx: Transaction) -> Result<Txid> {
        let wallet = self.wallet.lock().await;
        // TODO: Optimize this match to be a map_err / more readable in general
        let txid = tx.txid();
        match wallet.broadcast(tx) {
            Ok(txid) => Ok(txid),
            Err(e) => {
                if let Error::Electrum(electrum_client::Error::Protocol(ref value)) = e {
                    let error_code = match parse_rpc_protocol_error_code(value) {
                        Ok(error_code) => error_code,
                        Err(inner) => {
                            eprintln!("Failed to parse error code from RPC message: {}", inner);
                            return Err(anyhow!(e));
                        }
                    };

                    if error_code == i64::from(RpcErrorCode::RpcVerifyAlreadyInChain) {
                        return Ok(txid);
                    }
                }

                Err(anyhow!(e))
            }
        }
    }
}

fn parse_rpc_protocol_error_code(error_value: &Value) -> anyhow::Result<i64> {
    let json_map = match error_value {
        serde_json::Value::Object(map) => map,
        _ => bail!("Json error is not json object "),
    };

    let error_code_value = match json_map.get("code") {
        Some(val) => val,
        None => bail!("No error code field"),
    };

    let error_code_number = match error_code_value {
        serde_json::Value::Number(num) => num,
        _ => bail!("Error code is not a number"),
    };

    if let Some(int) = error_code_number.as_i64() {
        Ok(int)
    } else {
        bail!("Error code is not an unsigned integer")
    }
}

/// Bitcoin error codes: https://github.com/bitcoin/bitcoin/blob/97d3500601c1d28642347d014a6de1e38f53ae4e/src/rpc/protocol.h#L23
pub enum RpcErrorCode {
    /// Transaction or block was rejected by network rules. Error code -27.
    RpcVerifyAlreadyInChain,
}

impl From<RpcErrorCode> for i64 {
    fn from(code: RpcErrorCode) -> Self {
        match code {
            RpcErrorCode::RpcVerifyAlreadyInChain => -27,
        }
    }
}
