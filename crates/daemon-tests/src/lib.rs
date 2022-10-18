use crate::flow::ensure_null_next_offers;
use crate::flow::next_maker_offers;
use crate::flow::next_with;
use crate::flow::one_cfd_with_state;
use crate::mocks::monitor::MonitorActor;
use crate::mocks::oracle::dummy_wrong_attestation;
use crate::mocks::oracle::OracleActor;
use crate::mocks::price_feed::PriceFeedActor;
use crate::mocks::wallet::WalletActor;
use anyhow::Context;
use daemon::auto_rollover;
use daemon::bdk::bitcoin::Amount;
use daemon::bdk::bitcoin::Network;
use daemon::bdk::bitcoin::SignedAmount;
use daemon::bdk::bitcoin::Txid;
use daemon::libp2p_utils::create_connect_multiaddr;
use daemon::maia_core::secp256k1_zkp::SecretKey;
use daemon::maia_core::secp256k1_zkp::XOnlyPublicKey;
use daemon::online_status::ConnectionStatus;
use daemon::oracle::Attestation;
use daemon::projection;
use daemon::projection::Cfd;
use daemon::projection::CfdState;
use daemon::projection::FeedReceivers;
use daemon::projection::MakerOffers;
use daemon::seed::RandomSeed;
use daemon::seed::Seed;
use daemon::Environment;
use daemon::N_PAYOUTS;
use maia::olivia::btc_example_0;
use maia::OliviaData;
use maker::cfd::OfferParams;
use model::libp2p::PeerId;
use model::olivia::Announcement;
use model::olivia::BitMexPriceEventId;
use model::CfdEvent;
use model::CompleteFee;
use model::ContractSymbol;
use model::Contracts;
use model::Dlc;
use model::EventKind;
use model::FeeAccount;
use model::FundingFee;
use model::FundingRate;
use model::Identity;
use model::Leverage;
use model::LotSize;
use model::OpeningFee;
use model::OrderId;
use model::Position;
use model::Price;
use model::Role;
use model::TxFeeRate;
use model::SETTLEMENT_INTERVAL;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use std::collections::HashSet;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use time::OffsetDateTime;
use tokio::sync::watch;
use tokio_extras::Tasks;
use tracing::instrument;
use xtra::Actor;
use xtra_bitmex_price_feed::LatestQuotes;
use xtra_bitmex_price_feed::Quote;
use xtra_libp2p::libp2p::Multiaddr;
use xtra_libp2p::multiaddress_ext::MultiaddrExt;

pub mod flow;
pub mod maia;
pub mod mocks;
pub mod rollover;

#[macro_export]
macro_rules! confirm {
    (lock transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_lock_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_lock_transaction($id)
            .await;
    };
    (commit transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_commit_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_commit_transaction($id)
            .await;
    };
    (refund transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_refund_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_refund_transaction($id)
            .await;
    };
    (close transaction, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .confirm_close_transaction($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .confirm_close_transaction($id)
            .await;
    };
    (cet, $id:expr, $maker:expr, $taker:expr) => {
        $maker.mocks.monitor().await.confirm_cet($id).await;
        $taker.mocks.monitor().await.confirm_cet($id).await;
    };
}

#[macro_export]
macro_rules! expire {
    (cet timelock, $id:expr, $maker:expr, $taker:expr) => {
        $maker.mocks.monitor().await.expire_cet_timelock($id).await;
        $taker.mocks.monitor().await.expire_cet_timelock($id).await;
    };
    (refund timelock, $id:expr, $maker:expr, $taker:expr) => {
        $maker
            .mocks
            .monitor()
            .await
            .expire_refund_timelock($id)
            .await;
        $taker
            .mocks
            .monitor()
            .await
            .expire_refund_timelock($id)
            .await;
    };
}

/// Simulate oracle attestation for both actor systems
#[macro_export]
macro_rules! simulate_attestation {
    ($maker:expr, $taker:expr, $order_id:expr, $attestation:expr) => {{
        tracing::debug!("Simulating attestation: {:?}", $attestation);

        $maker
            .mocks
            .oracle()
            .await
            .simulate_attestation($order_id, $attestation)
            .await;

        $taker
            .mocks
            .oracle()
            .await
            .simulate_attestation($order_id, $attestation)
            .await;
    }};
}

/// Waits until the CFDs for both maker and taker are in the given state.
#[macro_export]
macro_rules! wait_next_state {
    ($id:expr, $maker:expr, $taker:expr, $maker_state:expr, $taker_state:expr) => {
        let wait_until_taker = next_with($taker.cfd_feed(), |maybe_cfds| {
            maybe_cfds.and_then(one_cfd_with_state($taker_state))
        });
        let wait_until_maker = next_with($maker.cfd_feed(), |maybe_cfds| {
            maybe_cfds.and_then(one_cfd_with_state($maker_state))
        });

        let (taker_cfd, maker_cfd) = tokio::join!(wait_until_taker, wait_until_maker);
        let taker_cfd = taker_cfd.unwrap();
        let maker_cfd = maker_cfd.unwrap();

        assert_eq!(
            taker_cfd.order_id, maker_cfd.order_id,
            "order id mismatch between maker and taker"
        );
        assert_eq!(taker_cfd.order_id, $id, "unexpected order id in the taker");
        assert_eq!(maker_cfd.order_id, $id, "unexpected order id in the maker");
        tracing::info!(?maker_cfd.state, "Current maker CFD state");
        tracing::info!(?taker_cfd.state, "Current taker CFD state");
    };
    ($id:expr, $maker:expr, $taker:expr, $state:expr) => {
        wait_next_state!($id, $maker, $taker, $state, $state)
    };
}

/// Waits until the CFDs with given order_id for both maker and taker are in the given state.
#[macro_export]
macro_rules! wait_next_state_multi_cfd {
    ($id:expr, $maker:expr, $taker:expr, $maker_state:expr, $taker_state:expr) => {
        let wait_until_taker = next_with($taker.cfd_feed(), |maybe_cfds| {
            maybe_cfds.and_then(cfd_with_state($id, $taker_state))
        });
        let wait_until_maker = next_with($maker.cfd_feed(), |maybe_cfds| {
            maybe_cfds.and_then(cfd_with_state($id, $maker_state))
        });

        let (taker_cfd, maker_cfd) = tokio::join!(wait_until_taker, wait_until_maker);
        let taker_cfd = taker_cfd.unwrap();
        let maker_cfd = maker_cfd.unwrap();

        assert_eq!(
            taker_cfd.order_id, maker_cfd.order_id,
            "order id mismatch between maker and taker"
        );
        assert_eq!(taker_cfd.order_id, $id, "unexpected order id in the taker");
        assert_eq!(maker_cfd.order_id, $id, "unexpected order id in the maker");
    };
    ($id:expr, $maker:expr, $taker:expr, $state:expr) => {
        wait_next_state_multi_cfd!($id, $maker, $taker, $state, $state)
    };
}

/// Arguments that need to be supplied to the `open_cfd` test helper.
#[derive(Clone)]
pub struct OpenCfdArgs {
    pub contract_symbol: ContractSymbol,
    pub position_maker: Position,
    pub initial_price: Price,
    pub quantity: Contracts,
    pub taker_leverage: Leverage,
    pub oracle_data: OliviaData,
}

pub fn initial_price_for(symbol: ContractSymbol) -> Price {
    Price::new(match symbol {
        ContractSymbol::BtcUsd => dummy_btc_price(),
        ContractSymbol::EthUsd => dummy_eth_price(),
    })
    .unwrap()
}

/// Different contract symbols can have different lot sizes
fn lot_size_for(symbol: ContractSymbol) -> LotSize {
    match symbol {
        ContractSymbol::BtcUsd => LotSize::new(100),
        ContractSymbol::EthUsd => LotSize::new(1),
    }
}

impl OpenCfdArgs {
    fn offer_params(&self) -> OfferParams {
        OfferParamsBuilder::new(self.contract_symbol)
            .price(self.initial_price)
            .build()
    }

    pub fn fee_calculator(&self) -> FeeCalculator {
        debug_assert!(self
            .offer_params()
            .leverage_choices
            .contains(&self.taker_leverage));

        FeeCalculator::new(
            self.contract_symbol,
            self.offer_params(),
            self.quantity,
            self.taker_leverage,
            self.position_maker,
        )
    }
}

impl Default for OpenCfdArgs {
    fn default() -> Self {
        let contract_symbol = ContractSymbol::BtcUsd;
        Self {
            contract_symbol,
            initial_price: initial_price_for(contract_symbol),
            position_maker: Position::Short,
            quantity: Contracts::new(100),
            taker_leverage: Leverage::TWO,
            oracle_data: btc_example_0(),
        }
    }
}

/// Open a CFD between `taker` and `maker`.
///
/// This allows callers to use it as a starting point for their test.
pub async fn open_cfd(taker: &mut Taker, maker: &mut Maker, args: OpenCfdArgs) -> OrderId {
    let offer_params = args.offer_params();
    let OpenCfdArgs {
        oracle_data,
        position_maker,
        quantity,
        taker_leverage,
        contract_symbol,
        ..
    } = args;

    ensure_null_next_offers(taker.offers_feed()).await.unwrap();

    tracing::debug!("Sending {offer_params:?}");
    maker.set_offer_params(offer_params).await;

    let (_, received) =
        next_maker_offers(maker.offers_feed(), taker.offers_feed(), &contract_symbol)
            .await
            .unwrap();

    tracing::debug!("Received from maker {received:?}");

    mock_oracle_announcements(maker, taker, oracle_data.announcements()).await;

    let offer_to_take = match (contract_symbol, position_maker) {
        (ContractSymbol::BtcUsd, Position::Long) => received.btcusd_long,
        (ContractSymbol::BtcUsd, Position::Short) => received.btcusd_short,
        (ContractSymbol::EthUsd, Position::Long) => received.ethusd_long,
        (ContractSymbol::EthUsd, Position::Short) => received.ethusd_short,
    }
    .context("Order for expected position not set")
    .unwrap();

    let offer_id = offer_to_take.id;

    let order_id = taker
        .system
        .place_order(offer_id, quantity, taker_leverage)
        .await
        .unwrap();
    wait_next_state!(order_id, maker, taker, CfdState::PendingSetup);

    maker.mocks.mock_party_params().await;
    taker.mocks.mock_party_params().await;

    maker.mocks.mock_wallet_sign_and_broadcast().await;
    taker.mocks.mock_wallet_sign_and_broadcast().await;

    maker.system.accept_order(order_id).await.unwrap();
    wait_next_state!(order_id, maker, taker, CfdState::ContractSetup);

    wait_next_state!(order_id, maker, taker, CfdState::PendingOpen);

    confirm!(lock transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Open);

    order_id
}

/// Settle a CFD non collaboratively.
///
/// It publishes the commit transaction; expires of the CET timelock on it; and simulates the
/// attestation of the oracle based on the `attestation` argument passed in, causing the publication
/// of the corresponding CET.
///
/// After every step, we check that the CFD goes through the correct state based on
/// `daemon::projection`. Furthermore, we check that making the oracle generate an attestation for a
/// different event has no effect on the CFD.
pub async fn settle_non_collaboratively(
    taker: &mut Taker,
    maker: &mut Maker,
    order_id: OrderId,
    attestation: &Attestation,
) {
    confirm!(commit transaction, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    // After CetTimelockExpired, we're only waiting for attestation
    expire!(cet timelock, order_id, maker, taker);

    // Delivering the wrong attestation does not move state to `PendingCet`
    simulate_attestation!(taker, maker, order_id, &dummy_wrong_attestation());
    wait_next_state!(order_id, maker, taker, CfdState::OpenCommitted);

    // Delivering correct attestation moves the state `PendingCet`
    simulate_attestation!(taker, maker, order_id, attestation);
    wait_next_state!(order_id, maker, taker, CfdState::PendingCet);

    confirm!(cet, order_id, maker, taker);
    wait_next_state!(order_id, maker, taker, CfdState::Closed);
}

pub struct FeeCalculator {
    /// Opening fee charged by the maker
    opening_fee: OpeningFee,

    /// Funding fee for the first 24h calculated when opening a Cfd
    initial_funding_fee: FundingFee,

    contract_symbol: ContractSymbol,
    maker_position: Position,
    offer_params: OfferParams,
    quantity: Contracts,
    taker_leverage: Leverage,
}

impl FeeCalculator {
    pub fn new(
        contract_symbol: ContractSymbol,
        offer_params: OfferParams,
        quantity: Contracts,
        taker_leverage: Leverage,
        maker_position: Position,
    ) -> Self {
        let initial_funding_fee = match maker_position {
            Position::Long => FundingFee::calculate(
                offer_params.price_long.unwrap(),
                quantity,
                Leverage::ONE,
                taker_leverage,
                offer_params.funding_rate_long,
                SETTLEMENT_INTERVAL.whole_hours(),
                contract_symbol,
            )
            .unwrap(),
            Position::Short => FundingFee::calculate(
                offer_params.price_short.unwrap(),
                quantity,
                taker_leverage,
                Leverage::ONE,
                offer_params.funding_rate_short,
                SETTLEMENT_INTERVAL.whole_hours(),
                contract_symbol,
            )
            .unwrap(),
        };

        Self {
            opening_fee: offer_params.opening_fee,
            initial_funding_fee,
            contract_symbol,
            maker_position,
            offer_params,
            quantity,
            taker_leverage,
        }
    }

    /// Calculates the complete fee based on given rollover hours
    ///
    /// Takes the initial funding fee (for the first 24h) and the opening fee and adds the rollover
    /// fees based on the hours supplied. This allows predicting the complete fees for a CFD for
    /// multiple rollovers based on the hours to be charged.
    pub fn complete_fee_for_rollover_hours(
        &self,
        accumulated_rollover_hours_to_charge: i64,
    ) -> (SignedAmount, SignedAmount) {
        if accumulated_rollover_hours_to_charge == 0 {
            tracing::info!("Predicting fees before first rollover")
        } else {
            tracing::info!(
                "Predicting fee for {} hours",
                accumulated_rollover_hours_to_charge
            );
        }

        tracing::debug!("Opening fee: {}", self.opening_fee.to_inner());

        let mut maker_fee_account = FeeAccount::new(self.maker_position, Role::Maker)
            .add_opening_fee(self.opening_fee)
            .add_funding_fee(self.initial_funding_fee);

        let taker_position = self.maker_position.counter_position();
        let mut taker_fee_account = FeeAccount::new(taker_position, Role::Taker)
            .add_opening_fee(self.opening_fee)
            .add_funding_fee(self.initial_funding_fee);

        tracing::debug!(
            "Maker fees including opening and initial funding fee: {}",
            maker_fee_account.balance()
        );

        tracing::debug!(
            "Taker fees including opening and initial funding fee: {}",
            taker_fee_account.balance()
        );

        let accumulated_hours_to_charge = match self.maker_position {
            Position::Long => FundingFee::calculate(
                self.offer_params.price_long.unwrap(),
                self.quantity,
                Leverage::ONE,
                self.taker_leverage,
                self.offer_params.funding_rate_long,
                accumulated_rollover_hours_to_charge,
                self.contract_symbol,
            )
            .unwrap(),
            Position::Short => FundingFee::calculate(
                self.offer_params.price_short.unwrap(),
                self.quantity,
                self.taker_leverage,
                Leverage::ONE,
                self.offer_params.funding_rate_short,
                accumulated_rollover_hours_to_charge,
                self.contract_symbol,
            )
            .unwrap(),
        };

        maker_fee_account = maker_fee_account.add_funding_fee(accumulated_hours_to_charge);
        taker_fee_account = taker_fee_account.add_funding_fee(accumulated_hours_to_charge);

        tracing::debug!(
            "Maker fees including all fees: {}",
            maker_fee_account.balance()
        );

        tracing::debug!(
            "Taker fees including all fees: {}",
            taker_fee_account.balance()
        );

        (maker_fee_account.balance(), taker_fee_account.balance())
    }

    pub fn complete_fee_for_expired_settlement_event(&self) -> (SignedAmount, SignedAmount) {
        // The expected to be charged for 24h because we only charge one full term
        // This is due to the rollover falling back to charging one full term if the event is
        // already past expiry.
        self.complete_fee_for_rollover_hours(24)
    }
}

fn oracle_pk() -> XOnlyPublicKey {
    XOnlyPublicKey::from_str("ddd4636845a90185991826be5a494cde9f4a6947b1727217afedc6292fa4caf7")
        .unwrap()
}

#[instrument]
pub async fn start_both() -> (Maker, Taker) {
    let maker = Maker::start(&MakerConfig::default()).await;
    let taker = Taker::start(
        &TakerConfig::default(),
        maker.identity,
        maker.connect_addr.clone(),
    )
    .await;
    (maker, taker)
}

#[derive(Clone, Debug)]
pub struct MakerConfig {
    oracle_pk: XOnlyPublicKey,
    seed: RandomSeed,
    n_payouts: usize,
    libp2p_port: u16,
    blocked_peers: HashSet<xtra_libp2p::libp2p::PeerId>,
}

impl Default for MakerConfig {
    fn default() -> Self {
        Self {
            oracle_pk: oracle_pk(),
            seed: RandomSeed::default(),
            n_payouts: N_PAYOUTS,
            libp2p_port: portpicker::pick_unused_port().expect("to be able to find a free port"),
            blocked_peers: HashSet::new(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct TakerConfig {
    oracle_pk: XOnlyPublicKey,
    seed: RandomSeed,
    n_payouts: usize,
}

impl Default for TakerConfig {
    fn default() -> Self {
        Self {
            oracle_pk: oracle_pk(),
            seed: RandomSeed::default(),
            n_payouts: N_PAYOUTS,
        }
    }
}

/// Maker Test Setup
pub struct Maker {
    pub system: maker::ActorSystem<OracleActor, WalletActor>,
    pub mocks: mocks::Mocks,
    pub feeds: FeedReceivers,
    pub listen_addr: SocketAddr,
    pub identity: Identity,
    /// The address on which taker can dial in with libp2p protocols (includes
    /// maker's PeerId)
    pub connect_addr: Multiaddr,
    _tasks: Tasks,
}

impl Maker {
    pub fn cfd_feed(&mut self) -> &mut watch::Receiver<Option<Vec<Cfd>>> {
        &mut self.feeds.cfds
    }

    pub fn first_cfd(&mut self) -> Cfd {
        self.cfd_feed()
            .borrow()
            .as_ref()
            .unwrap()
            .first()
            .unwrap()
            .clone()
    }

    pub fn cfds(&mut self) -> Vec<Cfd> {
        self.cfd_feed().borrow().as_ref().unwrap().clone()
    }

    pub fn latest_commit_txid(&mut self) -> Txid {
        self.first_cfd()
            .aggregated()
            .latest_dlc()
            .as_ref()
            .unwrap()
            .commit
            .0
            .txid()
    }

    pub fn latest_settlement_event_id(&mut self) -> BitMexPriceEventId {
        self.first_cfd()
            .aggregated()
            .latest_dlc()
            .as_ref()
            .unwrap()
            .settlement_event_id
    }

    pub fn latest_accumulated_fees(&mut self) -> SignedAmount {
        self.first_cfd().accumulated_fees
    }

    pub fn offers_feed(&mut self) -> &mut watch::Receiver<MakerOffers> {
        &mut self.feeds.offers
    }

    #[instrument(name = "Start maker", skip_all)]
    pub async fn start(config: &MakerConfig) -> Self {
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), config.libp2p_port);

        let db = sqlite_db::memory().await.unwrap();

        let (wallet, wallet_mock) = WalletActor::new();
        let (price_feed, price_feed_mock) = PriceFeedActor::new();

        let mut tasks = Tasks::default();

        let wallet_addr = wallet.create(None).spawn(&mut tasks);

        let (price_feed_addr, price_feed_fut) = price_feed.create(None).run();
        tasks.add(async move {
            let _ = price_feed_fut.await;
        });

        let settlement_interval = SETTLEMENT_INTERVAL;

        let identities = config.seed.derive_identities();

        let (projection_actor, projection_context) = xtra::Context::new(None);

        let mut monitor_mock = None;
        let mut oracle_mock = None;

        let endpoint_listen =
            daemon::libp2p_utils::create_listen_tcp_multiaddr(&address.ip(), address.port())
                .expect("to parse properly");

        let maker = maker::ActorSystem::new(
            db.clone(),
            wallet_addr,
            config.oracle_pk,
            |executor| {
                let (oracle, mock) = OracleActor::new(executor);
                oracle_mock = Some(mock);

                oracle
            },
            |executor| {
                let (monitor, mock) = MonitorActor::new(executor);
                monitor_mock = Some(mock);

                Ok(monitor)
            },
            settlement_interval,
            config.n_payouts,
            projection_actor,
            identities.clone(),
            endpoint_listen.clone(),
            config.blocked_peers.clone(),
        )
        .unwrap();

        let mocks = mocks::Mocks::new(
            wallet_mock,
            price_feed_mock,
            monitor_mock.unwrap(),
            oracle_mock.unwrap(),
        );

        let (feed_senders, feed_receivers) = projection::feeds();
        let feed_senders = Arc::new(feed_senders);
        let proj_actor = projection::Actor::new(
            db,
            Network::Testnet,
            price_feed_addr.into(),
            Role::Maker,
            feed_senders,
        );
        tasks.add(projection_context.run(proj_actor));

        Self {
            system: maker,
            feeds: feed_receivers,
            identity: model::Identity::new(identities.identity_pk),
            listen_addr: address,
            mocks,
            _tasks: tasks,
            connect_addr: create_connect_multiaddr(&endpoint_listen, &identities.peer_id().inner())
                .expect("to parse properly"),
        }
    }

    pub async fn set_offer_params(&mut self, offer_params: OfferParams) {
        let OfferParams {
            price_long,
            price_short,
            min_quantity,
            max_quantity,
            tx_fee_rate,
            funding_rate_long,
            funding_rate_short,
            opening_fee,
            leverage_choices,
            contract_symbol,
            lot_size,
        } = offer_params;
        self.system
            .set_offer_params(
                price_long,
                price_short,
                min_quantity,
                max_quantity,
                tx_fee_rate,
                funding_rate_long,
                funding_rate_short,
                opening_fee,
                leverage_choices,
                contract_symbol,
                lot_size,
            )
            .await
            .unwrap();
    }

    pub fn latest_revoked_revocation_sk_ours(&mut self) -> SecretKey {
        self.first_cfd()
            .aggregated()
            .latest_dlc()
            .as_ref()
            .unwrap()
            .revoked_commit
            .first()
            .unwrap()
            .revocation_sk_ours
            .unwrap()
    }
}

/// Taker Test Setup
pub struct Taker {
    pub id: Identity,
    pub system: daemon::TakerActorSystem<OracleActor, WalletActor, PriceFeedActor>,
    pub mocks: mocks::Mocks,
    pub feeds: FeedReceivers,
    pub maker_peer_id: PeerId,
    db: sqlite_db::Connection,
    _tasks: Tasks,
}

impl Taker {
    pub fn cfd_feed(&mut self) -> &mut watch::Receiver<Option<Vec<Cfd>>> {
        &mut self.feeds.cfds
    }

    pub fn first_cfd(&mut self) -> Cfd {
        self.cfd_feed()
            .borrow()
            .as_ref()
            .unwrap()
            .first()
            .unwrap()
            .clone()
    }

    pub fn cfds(&mut self) -> Vec<Cfd> {
        self.cfd_feed().borrow().as_ref().unwrap().clone()
    }

    pub fn latest_commit_txid(&mut self) -> Txid {
        self.first_cfd()
            .aggregated()
            .latest_dlc()
            .as_ref()
            .unwrap()
            .commit
            .0
            .txid()
    }

    pub fn latest_settlement_event_id(&mut self) -> BitMexPriceEventId {
        self.first_cfd()
            .aggregated()
            .latest_dlc()
            .as_ref()
            .unwrap()
            .settlement_event_id
    }

    pub fn latest_revoked_revocation_sk_theirs(&mut self) -> Option<SecretKey> {
        self.first_cfd()
            .aggregated()
            .latest_dlc()
            .as_ref()
            .unwrap()
            .revoked_commit
            .first()
            .unwrap()
            .revocation_sk_theirs
    }

    pub fn latest_dlc(&mut self) -> Dlc {
        self.first_cfd()
            .aggregated()
            .latest_dlc()
            .as_ref()
            .unwrap()
            .clone()
    }

    /// Exposes the `CompleteFee` as stored in the aggregate
    pub fn latest_complete_fee(&mut self) -> CompleteFee {
        self.first_cfd().aggregated().latest_fees()
    }

    /// Exposes the accumulated fees as exposed on the projection API
    pub fn latest_accumulated_fees(&mut self) -> SignedAmount {
        self.first_cfd().accumulated_fees
    }

    pub fn offers_feed(&mut self) -> &mut watch::Receiver<MakerOffers> {
        &mut self.feeds.offers
    }

    pub fn quote_feed(&mut self) -> &mut watch::Receiver<projection::LatestQuotes> {
        &mut self.feeds.quote
    }

    pub fn maker_status_feed(&mut self) -> &mut watch::Receiver<ConnectionStatus> {
        &mut self.system.maker_online_status_feed_receiver
    }

    #[instrument(name = "Start taker", skip_all)]
    pub async fn start(
        config: &TakerConfig,
        maker_identity: Identity,
        maker_multiaddr: Multiaddr,
    ) -> Self {
        let identities = config.seed.derive_identities();

        let db = sqlite_db::memory().await.unwrap();

        let mut tasks = Tasks::default();

        let (wallet, wallet_mock) = WalletActor::new();
        let wallet_addr = wallet.create(None).spawn(&mut tasks);

        let (price_feed, price_feed_mock) = PriceFeedActor::new();
        let (price_feed_addr, price_feed_fut) = price_feed.create(None).run();
        tasks.add(async move {
            let _ = price_feed_fut.await;
        });

        let (projection_actor, projection_context) = xtra::Context::new(None);

        let mut oracle_mock = None;
        let mut monitor_mock = None;
        tracing::info!("Connecting to maker {maker_multiaddr}");

        let taker = daemon::TakerActorSystem::new(
            db.clone(),
            wallet_addr,
            config.oracle_pk,
            identities.clone(),
            |executor| {
                let (oracle, mock) = OracleActor::new(executor);
                oracle_mock = Some(mock);

                oracle
            },
            |executor| {
                let (monitor, mock) = MonitorActor::new(executor);
                monitor_mock = Some(mock);

                Ok(monitor)
            },
            price_feed_addr,
            config.n_payouts,
            Duration::from_secs(10),
            projection_actor,
            maker_identity,
            maker_multiaddr.clone(),
            Environment::new("test"),
        )
        .unwrap();

        let mocks = mocks::Mocks::new(
            wallet_mock,
            price_feed_mock,
            monitor_mock.unwrap(),
            oracle_mock.unwrap(),
        );

        let (feed_senders, feed_receivers) = projection::feeds();
        let feed_senders = Arc::new(feed_senders);
        let proj_actor = projection::Actor::new(
            db.clone(),
            Network::Testnet,
            taker.price_feed_actor.clone().into(),
            Role::Taker,
            feed_senders,
        );
        tasks.add(projection_context.run(proj_actor));

        Self {
            id: model::Identity::new(identities.identity_pk),
            system: taker,
            feeds: feed_receivers,
            mocks,
            maker_peer_id: maker_multiaddr
                .extract_peer_id()
                .expect("to have peer id")
                .into(),
            db,
            _tasks: tasks,
        }
    }

    pub async fn trigger_rollover_with_latest_dlc_params(&mut self, order_id: OrderId) {
        let latest_dlc = self.first_cfd().aggregated().latest_dlc().clone().unwrap();
        self.system
            .auto_rollover_actor
            .send(auto_rollover::Rollover {
                order_id,
                maker_peer_id: Some(self.maker_peer_id),
                from_commit_txid: latest_dlc.commit.0.txid(),
                from_settlement_event_id: latest_dlc.settlement_event_id,
            })
            .await
            .unwrap();
    }

    pub async fn trigger_rollover_with_specific_params(
        &mut self,
        order_id: OrderId,
        from_commit_txid: Txid,
        from_settlement_event_id: BitMexPriceEventId,
    ) {
        self.system
            .auto_rollover_actor
            .send(auto_rollover::Rollover {
                order_id,
                maker_peer_id: Some(self.maker_peer_id),
                from_commit_txid,
                from_settlement_event_id,
            })
            .await
            .unwrap();
    }

    /// Appends an event that overwrites the current DLC
    ///
    /// To be used in tests that simulate a previous rollover state.
    /// Note that the projection does not get updated, this change only manipulates the database!
    /// When triggering another rollover this data will be loaded and used.
    pub async fn append_rollover_event(
        &mut self,
        id: OrderId,
        dlc: Dlc,
        complete_fee: CompleteFee,
    ) {
        tracing::info!(commit_txid = %dlc.commit.0.txid(), "Manually setting latest DLC");

        self.db
            .append_event(CfdEvent::new(
                id,
                EventKind::RolloverCompleted {
                    dlc: Some(dlc),
                    // Funding fee irrelevant because only CompleteFee is used
                    funding_fee: dummy_funding_fee(),
                    complete_fee: Some(complete_fee),
                },
            ))
            .await
            .unwrap()
    }
}

pub fn dummy_latest_quotes() -> LatestQuotes {
    vec![dummy_btc_quote(), dummy_eth_quote()]
        .iter()
        .map(|quote| (quote.symbol, *quote))
        .collect()
}

fn dummy_btc_quote() -> Quote {
    Quote {
        timestamp: OffsetDateTime::now_utc(),
        bid: dummy_btc_price(),
        ask: dummy_btc_price(),
        symbol: xtra_bitmex_price_feed::ContractSymbol::BtcUsd,
    }
}

fn dummy_eth_quote() -> Quote {
    Quote {
        timestamp: OffsetDateTime::now_utc(),
        bid: dummy_eth_price(),
        ask: dummy_eth_price(),
        symbol: xtra_bitmex_price_feed::ContractSymbol::EthUsd,
    }
}

pub struct OfferParamsBuilder(OfferParams);

impl OfferParamsBuilder {
    pub fn new(symbol: ContractSymbol) -> OfferParamsBuilder {
        let dummy_price = initial_price_for(symbol);

        OfferParamsBuilder(OfferParams {
            price_long: Some(dummy_price),
            price_short: Some(dummy_price),
            min_quantity: Contracts::new(100),
            max_quantity: Contracts::new(1000),
            tx_fee_rate: TxFeeRate::default(),
            // 8.76% annualized = rate of 0.0876 annualized = rate of 0.00024 daily
            funding_rate_long: FundingRate::new(dec!(0.00024)).unwrap(),
            funding_rate_short: FundingRate::new(dec!(0.00024)).unwrap(),
            opening_fee: OpeningFee::new(Amount::from_sat(2)),
            leverage_choices: vec![Leverage::TWO],
            contract_symbol: symbol,
            lot_size: lot_size_for(symbol),
        })
    }

    pub fn price(mut self, price: Price) -> Self {
        self.0.price_long = Some(price);
        self.0.price_short = Some(price);

        self
    }

    pub fn leverage_choices(mut self, choices: Vec<Leverage>) -> Self {
        self.0.leverage_choices = choices;

        self
    }

    pub fn build(self) -> OfferParams {
        self.0
    }
}

fn dummy_funding_fee() -> FundingFee {
    FundingFee::calculate(
        Price::new(dec!(10000)).unwrap(),
        Contracts::ZERO,
        Leverage::ONE,
        Leverage::ONE,
        Default::default(),
        0,
        ContractSymbol::BtcUsd,
    )
    .unwrap()
}

pub fn dummy_btc_price() -> Decimal {
    dec!(50_000)
}

pub fn dummy_eth_price() -> Decimal {
    dec!(1_500)
}

pub async fn mock_oracle_announcements(
    maker: &mut Maker,
    taker: &mut Taker,
    announcements: Vec<Announcement>,
) {
    taker
        .mocks
        .mock_oracle_announcement_with(announcements.clone())
        .await;
    maker
        .mocks
        .mock_oracle_announcement_with(announcements)
        .await;
}

pub async fn mock_quotes(maker: &mut Maker, taker: &mut Taker, contract_symbol: ContractSymbol) {
    taker.mocks.mock_latest_quotes().await;
    maker.mocks.mock_latest_quotes().await;
    let mut quote_receiver = taker.quote_feed().clone();

    let non_empty_quote = |quotes: projection::LatestQuotes| quotes.get(&contract_symbol).copied();

    next_with(&mut quote_receiver, non_empty_quote)
        .await
        .unwrap(); // if quote is available on feed, it propagated through the system
}
