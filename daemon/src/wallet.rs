use crate::model::WalletInfo;
use anyhow::{Context, Result};
use async_trait::async_trait;
use bdk::bitcoin::consensus::encode::serialize_hex;
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Amount, PublicKey, Transaction, Txid};
use bdk::blockchain::{ElectrumBlockchain, NoopProgress};
use bdk::wallet::AddressIndex;
use bdk::{electrum_client, KeychainKind, SignOptions};
use cfd_protocol::{PartyParams, WalletExt};
use rocket::serde::json::Value;
use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::Mutex;
use xtra_productivity::xtra_productivity;

#[derive(Clone)]
pub struct Actor {
    wallet: Arc<Mutex<bdk::Wallet<ElectrumBlockchain, bdk::database::SqliteDatabase>>>,
}

#[derive(thiserror::Error, Debug, Clone, Copy)]
#[error("The transaction is already in the blockchain")]
pub struct TransactionAlreadyInBlockchain;

impl Actor {
    pub async fn new(
        electrum_rpc_url: &str,
        wallet_dir: &Path,
        ext_priv_key: ExtendedPrivKey,
    ) -> Result<Self> {
        let client = bdk::electrum_client::Client::new(electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        let db = bdk::database::SqliteDatabase::new(wallet_dir.display().to_string());

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
}

#[xtra_productivity]
impl Actor {
    pub async fn handle_sync(&self, _msg: Sync) -> Result<WalletInfo> {
        let wallet = self.wallet.lock().await;
        wallet
            .sync(NoopProgress, None)
            .context("Failed to sync wallet")?;

        let balance = wallet.get_balance()?;

        let address = wallet.get_address(AddressIndex::LastUnused)?.address;

        let wallet_info = WalletInfo {
            balance: Amount::from_sat(balance),
            address,
            last_updated_at: SystemTime::now(),
        };

        Ok(wallet_info)
    }

    pub async fn handle_sign(&self, msg: Sign) -> Result<PartiallySignedTransaction> {
        let mut psbt = msg.psbt;
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

    pub async fn build_party_params(
        &self,
        BuildPartyParams {
            amount,
            identity_pk,
        }: BuildPartyParams,
    ) -> Result<PartyParams> {
        let wallet = self.wallet.lock().await;
        wallet.build_party_params(amount, identity_pk)
    }

    pub async fn handle_try_broadcast_transaction(
        &self,
        msg: TryBroadcastTransaction,
    ) -> Result<Txid> {
        let tx = msg.tx;
        let wallet = self.wallet.lock().await;
        let txid = tx.txid();

        let result = wallet.broadcast(tx.clone());

        if let Err(&bdk::Error::Electrum(electrum_client::Error::Protocol(ref value))) =
            result.as_ref()
        {
            let error_code = parse_rpc_protocol_error_code(value).with_context(|| {
                format!("Failed to parse electrum error response '{:?}'", value)
            })?;

            if error_code == i64::from(RpcErrorCode::RpcVerifyAlreadyInChain) {
                tracing::trace!(
                    %txid, "Attempted to broadcast transaction that was already on-chain",
                );

                return Ok(txid);
            }
        }

        let txid = result.with_context(|| {
            format!(
                "Broadcasting transaction failed. Txid: {}. Raw transaction: {}",
                txid,
                serialize_hex(&tx)
            )
        })?;

        Ok(txid)
    }
}

impl xtra::Actor for Actor {}

pub struct BuildPartyParams {
    pub amount: Amount,
    pub identity_pk: PublicKey,
}

pub struct Sync;

pub struct Sign {
    pub psbt: PartiallySignedTransaction,
}

pub struct TryBroadcastTransaction {
    pub tx: Transaction,
}

fn parse_rpc_protocol_error_code(error_value: &Value) -> Result<i64> {
    let json = error_value
        .as_str()
        .context("Not a string")?
        .split_terminator("RPC error: ")
        .nth(1)
        .context("Unknown error code format")?;

    let error = serde_json::from_str::<RpcError>(json).context("Error has unexpected format")?;

    Ok(error.code)
}

#[derive(serde::Deserialize)]
struct RpcError {
    code: i64,
}

/// Bitcoin error codes: <https://github.com/bitcoin/bitcoin/blob/97d3500601c1d28642347d014a6de1e38f53ae4e/src/rpc/protocol.h#L23>
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_error_response() {
        let response = serde_json::Value::String(r#"sendrawtransaction RPC error: {"code":-27,"message":"Transaction already in block chain"}"#.to_owned());

        let code = parse_rpc_protocol_error_code(&response).unwrap();

        assert_eq!(code, -27);
    }
}
