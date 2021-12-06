use crate::model::{Timestamp, WalletInfo};
use anyhow::{bail, Context, Result};
use bdk::bitcoin::consensus::encode::serialize_hex;
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::{Address, Amount, PublicKey, Script, Transaction, Txid};
use bdk::blockchain::{ElectrumBlockchain, NoopProgress};
use bdk::wallet::AddressIndex;
use bdk::{electrum_client, FeeRate, KeychainKind, SignOptions};
use maia::{PartyParams, WalletExt};
use rocket::serde::json::Value;

use std::path::Path;
use xtra_productivity::xtra_productivity;

const DUST_AMOUNT: u64 = 546;

pub struct Actor {
    wallet: bdk::Wallet<ElectrumBlockchain, bdk::database::SqliteDatabase>,
}

#[derive(thiserror::Error, Debug, Clone, Copy)]
#[error("The transaction is already in the blockchain")]
pub struct TransactionAlreadyInBlockchain;

impl Actor {
    pub fn new(
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

        Ok(Self { wallet })
    }

    /// Calculates the maximum "giveable" amount of this wallet.
    ///
    /// We define this as the maximum amount we can pay to a single output,
    /// given a fee rate.
    pub fn max_giveable(&self, locking_script_size: usize, fee_rate: FeeRate) -> Result<Amount> {
        let balance = self.wallet.get_balance()?;

        // TODO: Do we have to deal with the min_relay_fee here as well, i.e. if balance below
        // min_relay_fee we should return Amount::ZERO?
        if balance < DUST_AMOUNT {
            return Ok(Amount::ZERO);
        }

        let mut tx_builder = self.wallet.build_tx();

        let dummy_script = Script::from(vec![0u8; locking_script_size]);
        tx_builder.drain_to(dummy_script);
        tx_builder.fee_rate(fee_rate);
        tx_builder.drain_wallet();

        let response = tx_builder.finish();
        match response {
            Ok((_, details)) => {
                let max_giveable = details.sent
                    - details
                        .fee
                        .expect("fees are always present with Electrum backend");
                Ok(Amount::from_sat(max_giveable))
            }
            Err(bdk::Error::InsufficientFunds { .. }) => Ok(Amount::ZERO),
            Err(e) => bail!("Failed to build transaction. {:#}", e),
        }
    }
}

#[xtra_productivity]
impl Actor {
    pub fn handle_sync(&mut self, _msg: Sync) -> Result<WalletInfo> {
        self.wallet
            .sync(NoopProgress, None)
            .context("Failed to sync wallet")?;

        let balance = self.wallet.get_balance()?;

        let address = self.wallet.get_address(AddressIndex::LastUnused)?.address;

        let wallet_info = WalletInfo {
            balance: Amount::from_sat(balance),
            address,
            last_updated_at: Timestamp::now(),
        };

        Ok(wallet_info)
    }

    pub fn handle_sign(&mut self, msg: Sign) -> Result<PartiallySignedTransaction> {
        let mut psbt = msg.psbt;

        self.wallet
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

    pub fn build_party_params(
        &mut self,
        BuildPartyParams {
            amount,
            identity_pk,
        }: BuildPartyParams,
    ) -> Result<PartyParams> {
        self.wallet.build_party_params(amount, identity_pk)
    }

    pub fn handle_try_broadcast_transaction(
        &mut self,
        msg: TryBroadcastTransaction,
    ) -> Result<Txid> {
        let tx = msg.tx;
        let txid = tx.txid();

        let result = self.wallet.broadcast(&tx);

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

    pub fn handle_withdraw(&mut self, msg: Withdraw) -> Result<Txid> {
        if msg.address.network != self.wallet.network() {
            bail!(
                "Address has invalid network. It was {} but the wallet is connected to {}",
                msg.address.network,
                self.wallet.network()
            )
        }

        let fee_rate = msg.fee.unwrap_or_else(FeeRate::default_min_relay_fee);
        let address = msg.address;

        let amount = if let Some(amount) = msg.amount {
            amount
        } else {
            self.max_giveable(address.script_pubkey().len(), fee_rate)
                .context("Unable to drain wallet")?
        };

        tracing::info!(%amount, %address, "Amount to be sent to address");

        let mut tx_builder = self.wallet.build_tx();

        tx_builder
            .add_recipient(address.script_pubkey(), amount.as_sat())
            .fee_rate(fee_rate)
            // Turn on RBF signaling
            .enable_rbf();

        let (mut psbt, _) = tx_builder.finish()?;

        self.wallet.sign(&mut psbt, SignOptions::default())?;

        let txid = self.wallet.broadcast(&psbt.extract_tx())?;

        tracing::info!(%txid, "Withdraw successful");

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

pub struct Withdraw {
    pub amount: Option<Amount>,
    pub fee: Option<FeeRate>,
    pub address: Address,
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
