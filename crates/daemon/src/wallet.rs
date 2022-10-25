use crate::bitcoin::secp256k1::Secp256k1;
use crate::seed::RandomSeed;
use crate::seed::Seed;
use crate::seed::RANDOM_SEED_SIZE;
use crate::wallet::sled::Db;
use anyhow::anyhow;
use anyhow::bail;
use anyhow::ensure;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use bdk::bitcoin::blockdata::constants;
use bdk::bitcoin::hashes::Hash;
use bdk::bitcoin::util::bip32::ExtendedPrivKey;
use bdk::bitcoin::util::psbt::PartiallySignedTransaction;
use bdk::bitcoin::Address;
use bdk::bitcoin::Amount;
use bdk::bitcoin::BlockHash;
use bdk::bitcoin::Network;
use bdk::bitcoin::OutPoint;
use bdk::bitcoin::PublicKey;
use bdk::bitcoin::Txid;
use bdk::blockchain::Blockchain;
use bdk::blockchain::ElectrumBlockchain;
use bdk::database::BatchDatabase;
use bdk::electrum_client;
use bdk::electrum_client::ElectrumApi;
use bdk::sled;
use bdk::sled::Tree;
use bdk::wallet::tx_builder::TxOrdering;
use bdk::wallet::wallet_name_from_descriptor;
use bdk::wallet::AddressIndex;
use bdk::wallet::AddressInfo;
use bdk::FeeRate;
use bdk::KeychainKind;
use bdk::SignOptions;
use bdk::SyncOptions;
use bdk::Wallet;
use maia_core::PartyParams;
use maia_core::TxBuilderExt;
use model::Timestamp;
use model::TxFeeRate;
use model::WalletInfo;
use statrs::statistics::*;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use tokio::runtime::Handle;
use tokio::sync::watch;
use xtra::Actor as _;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

const SYNC_INTERVAL: Duration = Duration::from_secs(3 * 60);
pub const MAKER_WALLET_ID: &str = "maker-wallet";
pub const TAKER_WALLET_ID: &str = "taker-wallet";

static BALANCE_GAUGE: conquer_once::Lazy<prometheus::Gauge> = conquer_once::Lazy::new(|| {
    prometheus::register_gauge!(
        "wallet_balance_satoshis",
        "The sum of available UTXOs in the wallet in satoshis."
    )
    .unwrap()
});
static NUM_UTXO_GAUGE: conquer_once::Lazy<prometheus::Gauge> = conquer_once::Lazy::new(|| {
    prometheus::register_gauge!(
        "wallet_utxos_total",
        "The number of available UTXOs in the wallet."
    )
    .unwrap()
});
static MEDIAN_UTXO_VALUE_GAUGE: conquer_once::Lazy<prometheus::Gauge> =
    conquer_once::Lazy::new(|| {
        prometheus::register_gauge!(
            "wallet_median_utxo_satoshis",
            "The median UTXO, in satoshis."
        )
        .unwrap()
    });
static MIN_UTXO_VALUE_GAUGE: conquer_once::Lazy<prometheus::Gauge> =
    conquer_once::Lazy::new(|| {
        prometheus::register_gauge!(
            "wallet_min_utxo_satoshis",
            "The smallest UTXO, in satoshis."
        )
        .unwrap()
    });
static MAX_UTXO_VALUE_GAUGE: conquer_once::Lazy<prometheus::Gauge> =
    conquer_once::Lazy::new(|| {
        prometheus::register_gauge!("wallet_max_utxo_satoshis", "The largest UTXO, in satoshis.")
            .unwrap()
    });
static MEAN_UTXO_VALUE_GAUGE: conquer_once::Lazy<prometheus::Gauge> =
    conquer_once::Lazy::new(|| {
        prometheus::register_gauge!("wallet_mean_utxo_satoshis", "The mean UTXO, in satoshis.")
            .unwrap()
    });
static STD_DEV_UTXO_VALUE_GAUGE: conquer_once::Lazy<prometheus::Gauge> =
    conquer_once::Lazy::new(|| {
        prometheus::register_gauge!(
            "wallet_stddev_utxo_satoshis",
            "The standard deviation across all UTXOs, in satoshis."
        )
        .unwrap()
    });

pub struct Actor<B, DB> {
    wallet: Wallet<DB>,
    blockchain_client: B,
    used_utxos: LockedUtxos,
    sender: watch::Sender<Option<WalletInfo>>,
    db: Option<Db>,
    managed_wallet: bool,
}

impl Actor<ElectrumBlockchain, Tree> {
    pub fn spawn(
        electrum_rpc_url: &str,
        ext_priv_key: ExtendedPrivKey,
        db_path: PathBuf,
        managed_wallet: bool,
    ) -> Result<(xtra::Address<Self>, watch::Receiver<Option<WalletInfo>>)> {
        let client = electrum_client::Client::new(electrum_rpc_url)
            .context("Failed to initialize Electrum RPC client")?;

        ensure!(
            seed_and_rpc_on_same_network(&client, ext_priv_key.network)?,
            "Wallet seed and Electrum RPC client on different networks."
        );

        // Create a database (using default sled type) to store wallet data
        let db = sled::open(db_path)?;
        let wallet = Actor::build_wallet(ext_priv_key, db.clone())?;

        // UTXOs chosen after coin selection will only be locked for a
        // few wallet sync intervals. UTXOs which were actually
        // included in published transactions should be marked as
        // spent by the internal bdk wallet by then. UTXOs which ended
        // up not being used are expected to be safe to be reused by
        // then without incurring in double spend attempts.
        let time_to_lock = SYNC_INTERVAL * 4;

        let (sender, receiver) = watch::channel(None);

        let actor = Self {
            wallet,
            sender,
            used_utxos: LockedUtxos::new(time_to_lock),
            blockchain_client: ElectrumBlockchain::from(client),
            db: Some(db),
            managed_wallet,
        };

        let (addr, fut) = actor.create(None).run();
        let handle = Handle::current();
        std::thread::spawn(move || handle.block_on(fut));

        Ok((addr, receiver))
    }

    fn build_wallet(ext_priv_key: ExtendedPrivKey, db: Db) -> Result<Wallet<Tree>> {
        let wallet_name = wallet_name_from_descriptor(
            bdk::template::Bip84(ext_priv_key, KeychainKind::External),
            Some(bdk::template::Bip84(ext_priv_key, KeychainKind::Internal)),
            ext_priv_key.network,
            &Secp256k1::new(),
        )?;

        let db = db.open_tree(wallet_name)?;

        let wallet = Wallet::new(
            bdk::template::Bip84(ext_priv_key, KeychainKind::External),
            Some(bdk::template::Bip84(ext_priv_key, KeychainKind::Internal)),
            ext_priv_key.network,
            db,
        )?;

        Ok(wallet)
    }
}

#[xtra_productivity]
impl<B> Actor<B, Tree>
where
    Self: xtra::Actor,
{
    pub async fn import_seed(&mut self, msg: ImportSeed) -> Result<AddressInfo> {
        let seed = msg.seed;
        let import_seed: [u8; RANDOM_SEED_SIZE] = seed
            .clone()
            .try_into()
            .map_err(|_| anyhow!("seed must be {RANDOM_SEED_SIZE} bytes long"))?;

        let import_seed = RandomSeed::from(import_seed);
        let ext_priv_key = import_seed.derive_extended_priv_key(msg.network)?;

        // reuse existing database as the file has already been opened.
        let db = self.db.clone().expect("database should be existing.");

        // recreate and update wallet
        self.wallet = Actor::build_wallet(ext_priv_key, db)?;

        let name = msg.name;
        let wallet_seed = msg.path.join(&name);

        let now = Timestamp::now();
        let backup_seed = msg.path.join(format!("{name}.{now}.back"));

        if wallet_seed.exists() {
            // copying old seed file to safety.
            tokio::fs::copy(&wallet_seed, &backup_seed).await?;
        }

        // write imported seed file to disk.
        tokio::fs::write(wallet_seed.as_path(), seed).await?;

        self.wallet
            .get_address(AddressIndex::LastUnused)
            .map_err(|_| anyhow!("Could not get address"))
    }
}

impl<DB> Actor<ElectrumBlockchain, DB>
where
    DB: BatchDatabase,
{
    #[tracing::instrument(name = "Sync wallet", skip_all, err)]
    fn sync_internal(&mut self) -> Result<WalletInfo> {
        let now = Instant::now();
        tracing::trace!(target : "wallet", "Wallet sync started");

        tracing::debug_span!("Sync wallet database with blockchain").in_scope(|| {
            self.wallet
                .sync(&self.blockchain_client, SyncOptions::default())
                .context("Failed to sync wallet")
        })?;

        let balance =
            tracing::debug_span!("Get wallet balance").in_scope(|| self.wallet.get_balance())?;

        let balance = match self.wallet.network() {
            Network::Bitcoin => balance.get_spendable(),
            _ => balance.get_total(),
        };

        let utxo_values = tracing::debug_span!("Collect UTXO values").in_scope(|| {
            Ok::<_, bdk::Error>(Data::new(
                self.wallet
                    .list_unspent()?
                    .into_iter()
                    .map(|utxo| utxo.txout.value as f64)
                    .collect::<Vec<_>>(),
            ))
        })?;

        BALANCE_GAUGE.set(balance as f64);
        NUM_UTXO_GAUGE.set(utxo_values.len() as f64);
        MEDIAN_UTXO_VALUE_GAUGE.set(utxo_values.median());
        MIN_UTXO_VALUE_GAUGE.set(utxo_values.min());
        MAX_UTXO_VALUE_GAUGE.set(utxo_values.max());
        MEAN_UTXO_VALUE_GAUGE.set(utxo_values.mean().unwrap_or_default());
        STD_DEV_UTXO_VALUE_GAUGE.set(utxo_values.std_dev().unwrap_or_default());

        let address = self.wallet.get_address(AddressIndex::LastUnused)?.address;
        let transactions = self.wallet.list_transactions(false)?;

        let wallet_info = WalletInfo {
            network: self.wallet.network(),
            balance: Amount::from_sat(balance),
            address,
            last_updated_at: Timestamp::now(),
            transactions,
            managed_wallet: self.managed_wallet,
        };

        tracing::trace!(target : "wallet", sync_time_sec = %now.elapsed().as_secs(), "Wallet sync done");
        Ok(wallet_info)
    }
}

#[xtra_productivity]
impl<DB> Actor<ElectrumBlockchain, DB>
where
    DB: BatchDatabase,
{
    pub fn handle_sync(&mut self, _msg: Sync) {
        let wallet_info_update = match self.sync_internal() {
            Ok(wallet_info) => Some(wallet_info),
            Err(e) => {
                tracing::warn!("Syncing failed: {:#}", e);
                None
            }
        };
        let _ = self.sender.send(wallet_info_update);
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

        let tx = psbt.extract_tx();
        let txid = tx.txid();
        self.blockchain_client.broadcast(&tx)?;

        tracing::info!(%txid, "Withdraw successful");

        Ok(txid)
    }
}

#[xtra_productivity]
impl<B, DB> Actor<B, DB>
where
    Self: xtra::Actor,
    DB: BatchDatabase,
{
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
        let psbt = self
            .wallet
            .build_lock_tx(amount, &mut self.used_utxos, fee_rate.into())?;

        Ok(PartyParams {
            lock_psbt: psbt,
            identity_pk,
            lock_amount: amount,
            address: self.wallet.get_address(AddressIndex::New)?.address,
        })
    }
}

#[async_trait]
impl<DB: 'static> xtra::Actor for Actor<ElectrumBlockchain, DB>
where
    DB: BatchDatabase + Send,
{
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("self to be alive");

        tokio_extras::spawn(
            &this.clone(),
            this.send_interval(SYNC_INTERVAL, || Sync, xtras::IncludeSpan::Always),
        );
    }

    async fn stopped(self) -> Self::Stop {}
}

#[derive(Clone, Copy)]
pub struct BuildPartyParams {
    pub amount: Amount,
    pub identity_pk: PublicKey,
    pub fee_rate: TxFeeRate,
}

/// Message to trigger a sync.
#[derive(Clone, Copy)]
pub struct Sync;

pub struct Sign {
    pub psbt: PartiallySignedTransaction,
}

pub struct ImportSeed {
    pub seed: Vec<u8>,
    pub path: PathBuf,
    pub name: String,
    pub network: Network,
}

pub struct Withdraw {
    pub amount: Option<Amount>,
    pub fee: Option<FeeRate>,
    pub address: Address,
}

/// Bitcoin error codes: <https://github.com/bitcoin/bitcoin/blob/97d3500601c1d28642347d014a6de1e38f53ae4e/src/rpc/protocol.h#L23>
#[derive(Clone, Copy)]
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
        used_utxos: &mut LockedUtxos,
        fee_rate: FeeRate,
    ) -> Result<PartiallySignedTransaction>;
}

impl<DB> BuildLockTx for Wallet<DB>
where
    DB: BatchDatabase,
{
    fn build_lock_tx(
        &mut self,
        amount: Amount,
        used_utxos: &mut LockedUtxos,
        fee_rate: FeeRate,
    ) -> Result<PartiallySignedTransaction> {
        let mut builder = self.build_tx();

        builder
            .ordering(TxOrdering::Bip69Lexicographic) // TODO: I think this is pointless but we did this in maia.
            .fee_rate(fee_rate)
            .unspendable(used_utxos.list())
            .add_2of2_multisig_recipient(amount);

        let (psbt, _) = builder.finish()?;

        let used_inputs = psbt
            .unsigned_tx
            .input
            .iter()
            .map(|input| input.previous_output);
        used_utxos.extend(used_inputs);

        Ok(psbt)
    }
}

struct LockedUtxos {
    inner: HashSet<(Instant, OutPoint)>,
    time_to_lock: Duration,
}

impl LockedUtxos {
    fn new(time_to_lock: Duration) -> Self {
        Self {
            inner: HashSet::default(),
            time_to_lock,
        }
    }

    /// Add new elements to the set of locked UTXOs.
    fn extend<T: IntoIterator<Item = OutPoint>>(&mut self, utxos: T) {
        let now = Instant::now();
        let utxos = utxos.into_iter().map(|utxo| (now, utxo));

        self.inner.extend(utxos);
    }

    /// Return the list of locked UTXOs.
    ///
    /// Before creating the list, it removes all elements which should
    /// no longer be part of the set of locked UTXOs.
    fn list(&mut self) -> Vec<OutPoint> {
        self.remove_expired();
        self.inner.iter().map(|(_, utxo)| utxo).copied().collect()
    }

    /// Remove all elements in the set of locked UTXOs which have been
    /// stored for longer than `time_to_lock`.
    fn remove_expired(&mut self) {
        let now = Instant::now();

        self.inner = self
            .inner
            .drain()
            .skip_while(|(locked_at, _)| now >= *locked_at + self.time_to_lock)
            .collect();
    }
}

/// Compare the hash of the genesis block of the electrum RPC endpoint to the expected network's
/// genesis block hash. If they differ, the electrum RPC is not for the network that we expect.
fn seed_and_rpc_on_same_network(rpc: &electrum_client::Client, network: Network) -> Result<bool> {
    let network_hash = constants::genesis_block(network).block_hash();
    let mut hash = rpc.server_features()?.genesis_hash;
    hash.reverse(); // Sha256d hashes are displayed backwards
    let rpc_hash = BlockHash::from_slice(&hash)
        .context("Invalid genesis block hash returned by electrum RPC")?;

    Ok(network_hash == rpc_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bdk::database::MemoryDatabase;
    use bdk_ext::keypair;
    use bdk_ext::new_test_wallet;
    use bdk_ext::new_test_wallet_from_database;
    use itertools::Itertools;
    use rand::distributions::Alphanumeric;
    use rand::thread_rng;
    use rand::Rng;
    use std::collections::HashSet;
    use std::env;
    use std::path::Path;
    use tokio_extras::Tasks;

    impl<DB: 'static> Actor<(), DB>
    where
        DB: BatchDatabase,
    {
        pub fn new_offline<D>(
            utxo_amount: Amount,
            num_utxos: u8,
            time_to_lock: Duration,
            database: DB,
        ) -> Result<Self> {
            let wallet =
                new_test_wallet_from_database(&mut thread_rng(), utxo_amount, num_utxos, database)?;

            let (sender, _receiver) = watch::channel(None);

            Ok(Self {
                wallet,
                sender,
                used_utxos: LockedUtxos {
                    inner: HashSet::default(),
                    time_to_lock,
                },
                blockchain_client: (),
                db: None,
                managed_wallet: true,
            })
        }
    }

    #[async_trait]
    impl<DB: 'static> xtra::Actor for Actor<(), DB>
    where
        DB: Send,
    {
        type Stop = ();

        async fn stopped(self) -> Self::Stop {}
    }

    pub async fn create_random_folder() -> Result<PathBuf> {
        // create temporary folder
        let random_folder: String = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(7)
            .map(char::from)
            .collect();
        let data_dir = env::temp_dir().join(Path::new(&random_folder));
        tokio::fs::create_dir_all(&data_dir).await?;

        Ok(data_dir)
    }

    #[tokio::test]
    async fn import_itchysats_seed_with_invalid_seed_length() {
        let data_dir = create_random_folder()
            .await
            .expect("could not create random temp folder");

        let mut tasks = Tasks::default();

        let database = sled::open(data_dir.clone()).expect("could not open database");
        let database = database
            .open_tree("wallet name")
            .expect("could not open tree");

        // create wallet with only one UTXO which will be locked for a
        // long time after being used
        let actor =
            Actor::new_offline::<Tree>(Amount::ONE_BTC, 1, Duration::from_secs(120), database)
                .unwrap()
                .create(None)
                .spawn(&mut tasks);

        // generate empty export seed (e.g. umbrel like)
        let mut import_seed = [0u8; 32];
        thread_rng().fill(&mut import_seed);

        // import seed
        actor
            .send(ImportSeed {
                seed: import_seed.to_vec(),
                network: Network::Testnet,
                path: data_dir,
                name: crate::seed::TAKER_WALLET_SEED_FILE.to_string(),
            })
            .await
            .unwrap()
            .expect_err("seed should be too short");
    }

    #[tokio::test]
    async fn import_itchysats_seed() {
        let data_dir = create_random_folder()
            .await
            .expect("could not create random temp folder");

        let mut tasks = Tasks::default();

        let db = sled::open(data_dir.clone()).expect("could not open database");
        let database = db.open_tree("wallet name").expect("could not open tree");

        // create wallet with only one UTXO which will be locked for a
        // long time after being used
        let mut actor =
            Actor::new_offline::<Tree>(Amount::ONE_BTC, 1, Duration::from_secs(120), database)
                .unwrap();

        actor.db = Some(db);

        let old_address = &actor
            .wallet
            .get_address(AddressIndex::LastUnused)
            .expect("old address")
            .address;

        let wallet = actor.create(None).spawn(&mut tasks);

        const IMPORT_SEED: [u8; 256] = [
            137, 78, 181, 39, 89, 143, 9, 224, 92, 125, 51, 183, 87, 95, 206, 236, 135, 33, 54, 10,
            237, 169, 132, 74, 230, 66, 244, 244, 89, 224, 23, 62, 163, 60, 39, 66, 19, 213, 68,
            56, 125, 14, 244, 247, 52, 68, 200, 153, 127, 86, 77, 119, 239, 133, 59, 129, 112, 250,
            77, 36, 101, 31, 45, 181, 13, 55, 159, 179, 107, 50, 70, 70, 168, 192, 23, 51, 70, 84,
            108, 101, 244, 99, 105, 36, 227, 118, 221, 245, 104, 33, 3, 91, 176, 176, 157, 42, 201,
            92, 99, 104, 235, 84, 237, 229, 131, 204, 227, 15, 223, 109, 8, 47, 155, 132, 236, 108,
            106, 59, 185, 70, 20, 247, 133, 192, 164, 93, 65, 141, 16, 18, 105, 251, 23, 168, 95,
            145, 252, 101, 27, 218, 97, 26, 20, 71, 102, 221, 182, 146, 148, 11, 117, 159, 67, 132,
            223, 245, 141, 174, 157, 226, 67, 5, 61, 218, 98, 131, 8, 170, 233, 140, 139, 198, 24,
            107, 60, 147, 236, 194, 234, 227, 58, 54, 61, 245, 94, 209, 12, 28, 131, 156, 37, 169,
            32, 38, 38, 212, 2, 26, 204, 231, 154, 194, 38, 142, 30, 71, 33, 100, 164, 169, 168,
            34, 6, 110, 126, 22, 120, 191, 168, 219, 64, 70, 172, 224, 218, 142, 135, 179, 79, 140,
            174, 157, 60, 43, 141, 218, 178, 45, 30, 184, 79, 100, 43, 97, 228, 35, 122, 60, 143,
            239, 109, 104, 56, 64, 3, 131,
        ]; // holds 0.001 btc on testnet

        let address_info = wallet
            .send(ImportSeed {
                seed: IMPORT_SEED.to_vec(),
                network: Network::Testnet,
                path: data_dir,
                name: crate::seed::TAKER_WALLET_SEED_FILE.to_string(),
            })
            .await
            .unwrap()
            .expect("failed to import seed");

        assert_ne!(old_address.to_string(), address_info.address.to_string());
        assert_eq!(
            "tb1q6nn7xprjrztheedzy5353vv22nr43p8u8zamcc",
            address_info.address.to_string()
        );
    }

    #[test]
    fn creating_two_lock_transactions_uses_different_utxos() {
        let mut wallet = new_test_wallet(&mut thread_rng(), Amount::from_sat(1000), 10).unwrap();
        let mut used_utxos = LockedUtxos {
            inner: HashSet::default(),
            time_to_lock: Duration::from_secs(120),
        };

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
                .unsigned_tx
                .input
                .iter()
                .map(|i| i.previous_output),
        );
        utxos_in_transaction.extend(
            lock_tx_2
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
        assert_eq!(
            utxos_in_transaction.iter().sorted().collect::<Vec<_>>(),
            used_utxos.list().iter().sorted().collect::<Vec<_>>(),
        );
    }

    #[tokio::test]
    async fn utxo_is_locked_after_building_party_params() {
        let mut tasks = Tasks::default();

        let actor = Actor::new_offline::<MemoryDatabase>(
            Amount::ONE_BTC,
            1,
            Duration::from_secs(120),
            MemoryDatabase::new(),
        )
        .unwrap()
        .create(None)
        .spawn(&mut tasks);

        let (_, identity_pk) = keypair::new(&mut thread_rng());

        // building party params locks our only UTXO
        actor
            .send(BuildPartyParams {
                amount: Amount::from_btc(0.2).unwrap(),
                identity_pk,
                fee_rate: TxFeeRate::default(),
            })
            .await
            .unwrap()
            .expect("single UTXO to be available");

        // our only UTXO remains locked, so the second attempt at
        // building party params fails
        actor
            .send(BuildPartyParams {
                amount: Amount::from_btc(0.2).unwrap(),
                identity_pk,
                fee_rate: TxFeeRate::default(),
            })
            .await
            .unwrap()
            .expect_err("single UTXO to remain locked");
    }

    #[tokio::test]
    async fn utxo_can_be_unlocked_after_marking_as_unspendable() {
        let mut tasks = Tasks::default();

        // create wallet with only one UTXO which will be locked for a
        // few seconds after being used
        let time_to_lock = Duration::from_secs(2);
        let actor = Actor::new_offline::<MemoryDatabase>(
            Amount::ONE_BTC,
            1,
            time_to_lock,
            MemoryDatabase::new(),
        )
        .unwrap()
        .create(None)
        .spawn(&mut tasks);

        let (_, identity_pk) = keypair::new(&mut thread_rng());

        // building party params locks our only UTXO
        actor
            .send(BuildPartyParams {
                amount: Amount::from_btc(0.2).unwrap(),
                identity_pk,
                fee_rate: TxFeeRate::default(),
            })
            .await
            .unwrap()
            .expect("single UTXO to be available");

        // our only UTXO remains locked, so the second attempt at
        // building party params fails
        actor
            .send(BuildPartyParams {
                amount: Amount::from_btc(0.2).unwrap(),
                identity_pk,
                fee_rate: TxFeeRate::default(),
            })
            .await
            .unwrap()
            .expect_err("single UTXO to remain locked");

        // wait for lock on UTXO to expire
        tokio_extras::time::sleep(time_to_lock).await;

        // after enough time has passed, our UTXO can once again be
        // used to build party params
        let _party_params = actor
            .send(BuildPartyParams {
                amount: Amount::from_btc(0.2).unwrap(),
                identity_pk,
                fee_rate: TxFeeRate::default(),
            })
            .await
            .unwrap()
            .expect("single UTXO to be available after unlocking it");
    }
}
