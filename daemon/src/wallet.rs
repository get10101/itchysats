use crate::model::Timestamp;
use crate::model::WalletInfo;
use crate::xtra_ext::SendInterval;
use crate::Tasks;
use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::Address;
use bdk::bitcoin::Amount;
use bdk::bitcoin::OutPoint;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Txid;
use bdk::blockchain::ElectrumBlockchain;
use bdk::blockchain::NoopProgress;
use bdk::database::BatchDatabase;
use bdk::wallet::tx_builder::TxOrdering;
use bdk::wallet::AddressIndex;
use bdk::FeeRate;
use bdk::KeychainKind;
use bdk::SignOptions;
use maia::PartyParams;
use maia::TxBuilderExt;
use std::collections::HashSet;
use std::time::Duration;
use tokio::sync::watch;
use xtra_productivity::xtra_productivity;

pub struct Actor {
    wallet: bdk::Wallet<ElectrumBlockchain, bdk::database::MemoryDatabase>,
    used_utxos: HashSet<OutPoint>,
    tasks: Tasks,
    sender: watch::Sender<Option<WalletInfo>>,
}

#[derive(thiserror::Error, Debug, Clone, Copy)]
#[error("The transaction is already in the blockchain")]
pub struct TransactionAlreadyInBlockchain;

impl Actor {
    pub fn new(
        electrum_rpc_url: &str,
        ext_priv_key: ExtendedPrivKey,
    ) -> Result<(Self, watch::Receiver<Option<WalletInfo>>)> {
        let client = bdk::electrum_client::Client::new(electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        let db = bdk::database::MemoryDatabase::new();

        let wallet = bdk::Wallet::new(
            bdk::template::Bip84(ext_priv_key, KeychainKind::External),
            Some(bdk::template::Bip84(ext_priv_key, KeychainKind::Internal)),
            ext_priv_key.network,
            db,
            ElectrumBlockchain::from(client),
        )?;

        let (sender, receiver) = watch::channel(None);
        let actor = Self {
            wallet,
            tasks: Tasks::default(),
            sender,
            used_utxos: HashSet::default(),
        };

        Ok((actor, receiver))
    }

    fn sync_internal(&mut self) -> Result<WalletInfo> {
        self.wallet
            .sync(NoopProgress, Some(1000))
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
}

#[xtra_productivity]
impl Actor {
    pub fn handle_sync(&mut self, _msg: Sync) {
        let wallet_info_update = match self.sync_internal() {
            Ok(wallet_info) => Some(wallet_info),
            Err(e) => {
                tracing::debug!("{:#}", e);

                None
            }
        };

        let _ = self.sender.send(wallet_info_update);
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
            fee_rate,
        }: BuildPartyParams,
    ) -> Result<PartyParams> {
        let psbt = self.wallet.build_lock_tx(
            amount,
            &mut self.used_utxos,
            FeeRate::from_sat_per_vb(fee_rate as f32),
        )?;

        Ok(PartyParams {
            lock_psbt: psbt,
            identity_pk,
            lock_amount: amount,
            address: self.wallet.get_address(AddressIndex::New)?.address,
        })
    }

    pub fn handle_withdraw(&mut self, msg: Withdraw) -> Result<Txid> {
        self.sync_internal()?;

        if msg.address.network != self.wallet.network() {
            bail!(
                "Address has invalid network. It was {} but the wallet is connected to {}",
                msg.address.network,
                self.wallet.network()
            )
        }

        let fee_rate = msg.fee.unwrap_or_else(FeeRate::default_min_relay_fee);
        let address = msg.address;

        let mut psbt = {
            let mut tx_builder = self.wallet.build_tx();

            tx_builder
                .fee_rate(fee_rate)
                // Turn on RBF signaling
                .enable_rbf();

            match msg.amount {
                Some(amount) => {
                    tracing::info!(%amount, %address, "Withdrawing from wallet");

                    tx_builder.add_recipient(address.script_pubkey(), amount.as_sat());
                }
                None => {
                    tracing::info!(%address, "Draining wallet");

                    tx_builder.drain_wallet().drain_to(address.script_pubkey());
                }
            }

            let (psbt, _) = tx_builder.finish()?;

            psbt
        };

        self.wallet.sign(&mut psbt, SignOptions::default())?;
        let txid = self.wallet.broadcast(&psbt.extract_tx())?;

        tracing::info!(%txid, "Withdraw successful");

        Ok(txid)
    }
}

#[async_trait]
impl xtra::Actor for Actor {
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("self to be alive");

        self.tasks
            .add(this.send_interval(Duration::from_secs(10), || Sync));
    }
}

#[derive(Debug)]
pub struct BuildPartyParams {
    pub amount: Amount,
    pub identity_pk: PublicKey,
    pub fee_rate: u32,
}

/// Private message to trigger a sync.
#[derive(Debug)]
struct Sync;

#[derive(Debug)]
pub struct Sign {
    pub psbt: PartiallySignedTransaction,
}

#[derive(Debug)]
pub struct Withdraw {
    pub amount: Option<Amount>,
    pub fee: Option<FeeRate>,
    pub address: Address,
}

/// Bitcoin error codes: <https://github.com/bitcoin/bitcoin/blob/97d3500601c1d28642347d014a6de1e38f53ae4e/src/rpc/protocol.h#L23>
pub enum RpcErrorCode {
    /// General error during transaction or block submission Error code -25.
    RpcVerifyError,
    /// Transaction already in chain. Error code -27.
    RpcVerifyAlreadyInChain,
}

impl From<RpcErrorCode> for i64 {
    fn from(code: RpcErrorCode) -> Self {
        match code {
            RpcErrorCode::RpcVerifyError => -25,
            RpcErrorCode::RpcVerifyAlreadyInChain => -27,
        }
    }
}

/// Module private trait to faciliate testing.
///
/// Implementing this generically on `bdk::Wallet` allows us to call it on a dummy wallet in the
/// test.
trait BuildLockTx {
    fn build_lock_tx(
        &mut self,
        amount: Amount,
        used_utxos: &mut HashSet<OutPoint>,
        fee_rate: FeeRate,
    ) -> Result<PartiallySignedTransaction>;
}

impl<B, D> BuildLockTx for bdk::Wallet<B, D>
where
    D: BatchDatabase,
{
    fn build_lock_tx(
        &mut self,
        amount: Amount,
        used_utxos: &mut HashSet<OutPoint>,
        fee_rate: FeeRate,
    ) -> Result<PartiallySignedTransaction> {
        let mut builder = self.build_tx();

        builder
            .ordering(TxOrdering::Bip69Lexicographic) // TODO: I think this is pointless but we did this in maia.
            .fee_rate(fee_rate)
            .unspendable(used_utxos.iter().copied().collect())
            .add_2of2_multisig_recipient(amount);

        let (psbt, _) = builder.finish()?;

        let used_inputs = psbt
            .global
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output);
        used_utxos.extend(used_inputs);

        Ok(psbt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bdk_ext::new_test_wallet;
    use rand::thread_rng;
    use std::collections::HashSet;

    #[test]
    fn creating_two_lock_transactions_uses_different_utxos() {
        let mut wallet = new_test_wallet(&mut thread_rng(), Amount::from_sat(1000), 10).unwrap();
        let mut used_utxos = HashSet::new();

        let lock_tx_1 = wallet
            .build_lock_tx(
                Amount::from_sat(2500),
                &mut used_utxos,
                FeeRate::default_min_relay_fee(),
            )
            .unwrap();
        let lock_tx_2 = wallet
            .build_lock_tx(
                Amount::from_sat(2500),
                &mut used_utxos,
                FeeRate::default_min_relay_fee(),
            )
            .unwrap();

        let mut utxos_in_transaction = HashSet::new();
        utxos_in_transaction.extend(
            lock_tx_1
                .global
                .unsigned_tx
                .input
                .iter()
                .map(|i| i.previous_output),
        );
        utxos_in_transaction.extend(
            lock_tx_2
                .global
                .unsigned_tx
                .input
                .iter()
                .map(|i| i.previous_output),
        );

        // 2 TX a 2500 sats with UTXOs worth 1000s = 6 inputs
        // If there are 6 UTXOs in the HashSet, we know that they are all different (HashSets don't
        // allow duplicates!)
        let expected_num_utxos = 6;

        assert_eq!(utxos_in_transaction.len(), expected_num_utxos);
        assert_eq!(utxos_in_transaction, used_utxos);
    }
}
