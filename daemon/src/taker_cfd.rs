use crate::actors::log_error;
use crate::cfd_feed::CfdFeed;
use crate::db::{
    insert_cfd, insert_new_cfd_state_by_order_id, insert_order, load_cfd_by_order_id,
    load_order_by_id,
};
use crate::model::cfd::{
    Cfd, CfdState, CfdStateChangeEvent, CfdStateCommon, Dlc, Order, OrderId, Origin,
};
use crate::model::Usd;
use crate::monitor::{self, MonitorParams};
use crate::wallet::Wallet;
use crate::wire::SetupMsg;
use crate::{bitmex_price_feed, send_to_socket, setup_contract, wire};
use anyhow::{Context as _, Result};
use async_trait::async_trait;
use bdk::bitcoin::secp256k1::schnorrsig;
use futures::channel::mpsc;
use futures::{future, SinkExt};
use std::time::SystemTime;
use tokio::sync::watch;
use xtra::prelude::*;
use xtra::KeepRunning;

pub struct TakeOffer {
    pub order_id: OrderId,
    pub quantity: Usd,
}

pub struct MakerStreamMessage {
    pub item: Result<wire::MakerToTaker>,
}

pub struct CfdSetupCompleted {
    pub order_id: OrderId,
    pub dlc: Result<Dlc>,
}

pub struct PriceUpdate(pub bitmex_price_feed::Quote);

enum SetupState {
    Active {
        sender: mpsc::UnboundedSender<wire::SetupMsg>,
    },
    None,
}

pub struct Actor {
    db: sqlx::SqlitePool,
    wallet: Wallet,
    oracle_pk: schnorrsig::PublicKey,
    cfd_feed: CfdFeed,
    order_feed_actor_inbox: watch::Sender<Option<Order>>,
    send_to_maker: Address<send_to_socket::Actor<wire::TakerToMaker>>,
    monitor_actor: Address<monitor::Actor<Actor>>,
    setup_state: SetupState,
}

impl Actor {
    #[allow(clippy::too_many_arguments)]
    pub async fn new(
        db: sqlx::SqlitePool,
        wallet: Wallet,
        oracle_pk: schnorrsig::PublicKey,
        cfd_feed: CfdFeed,
        order_feed_actor_inbox: watch::Sender<Option<Order>>,
        send_to_maker: Address<send_to_socket::Actor<wire::TakerToMaker>>,
        monitor_actor: Address<monitor::Actor<Actor>>,
        cfds: Vec<Cfd>,
    ) -> Result<Self> {
        let mut conn = db.acquire().await?;

        // populate the CFD feed with existing CFDs
        cfd_feed.update(&mut conn).await?;

        for dlc in cfds.iter().filter_map(|cfd| Cfd::pending_open_dlc(cfd)) {
            let txid = wallet.try_broadcast_transaction(dlc.lock.0.clone()).await?;

            tracing::info!("Lock transaction published with txid {}", txid);
        }

        for cfd in cfds.iter().filter(|cfd| Cfd::is_must_refund(cfd)) {
            let signed_refund_tx = cfd.refund_tx()?;
            let txid = wallet.try_broadcast_transaction(signed_refund_tx).await?;

            tracing::info!("Refund transaction published on chain: {}", txid);
        }

        Ok(Self {
            db,
            wallet,
            oracle_pk,
            cfd_feed,
            order_feed_actor_inbox,
            send_to_maker,
            monitor_actor,
            setup_state: SetupState::None,
        })
    }

    async fn handle_take_offer(&mut self, order_id: OrderId, quantity: Usd) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        let current_order = load_order_by_id(order_id, &mut conn).await?;

        tracing::info!("Taking current order: {:?}", &current_order);

        let cfd = Cfd::new(
            current_order.clone(),
            quantity,
            CfdState::OutgoingOrderRequest {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
        );

        insert_cfd(cfd, &mut conn).await?;

        self.cfd_feed.update(&mut conn).await?;
        self.send_to_maker
            .do_send_async(wire::TakerToMaker::TakeOrder { order_id, quantity })
            .await?;

        Ok(())
    }

    async fn handle_new_order(&mut self, order: Option<Order>) -> Result<()> {
        match order {
            Some(mut order) => {
                order.origin = Origin::Theirs;

                let mut conn = self.db.acquire().await?;
                insert_order(&order, &mut conn).await?;
                self.order_feed_actor_inbox.send(Some(order))?;
            }
            None => {
                self.order_feed_actor_inbox.send(None)?;
            }
        }
        Ok(())
    }

    async fn handle_order_accepted(
        &mut self,
        order_id: OrderId,
        ctx: &mut Context<Self>,
    ) -> Result<()> {
        tracing::info!(%order_id, "Order got accepted");

        let (sender, receiver) = mpsc::unbounded();

        if let SetupState::Active { .. } = self.setup_state {
            anyhow::bail!("Already setting up a contract!")
        }

        let mut conn = self.db.acquire().await?;
        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::ContractSetup {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            &mut conn,
        )
        .await?;

        self.cfd_feed.update(&mut conn).await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let contract_future = setup_contract::new(
            self.send_to_maker
                .clone()
                .into_sink()
                .with(|msg| future::ok(wire::TakerToMaker::Protocol(msg))),
            receiver,
            self.oracle_pk,
            cfd,
            self.wallet.clone(),
            setup_contract::Role::Taker,
        );

        let this = ctx
            .address()
            .expect("actor to be able to give address to itself");

        tokio::spawn(async move {
            let dlc = contract_future.await;

            this.do_send_async(CfdSetupCompleted { order_id, dlc })
                .await
        });

        self.setup_state = SetupState::Active { sender };

        Ok(())
    }

    async fn handle_order_rejected(&mut self, order_id: OrderId) -> Result<()> {
        tracing::debug!(%order_id, "Order rejected");

        let mut conn = self.db.acquire().await?;
        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::Rejected {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
            },
            &mut conn,
        )
        .await?;

        self.cfd_feed.update(&mut conn).await?;

        Ok(())
    }

    async fn handle_inc_protocol_msg(&mut self, msg: SetupMsg) -> Result<()> {
        match &mut self.setup_state {
            SetupState::Active { sender } => {
                sender.send(msg).await?;
            }
            SetupState::None => anyhow::bail!("OrderAccepted message should arrive first"),
        }

        Ok(())
    }

    async fn handle_cfd_setup_completed(
        &mut self,
        order_id: OrderId,
        dlc: Result<Dlc>,
    ) -> Result<()> {
        self.setup_state = SetupState::None;
        let dlc = dlc.context("Failed to setup contract with maker")?;

        tracing::info!("Setup complete, publishing on chain now");

        let mut conn = self.db.acquire().await?;

        insert_new_cfd_state_by_order_id(
            order_id,
            CfdState::PendingOpen {
                common: CfdStateCommon {
                    transition_timestamp: SystemTime::now(),
                },
                dlc: dlc.clone(),
            },
            &mut conn,
        )
        .await?;

        self.cfd_feed.update(&mut conn).await?;

        let txid = self
            .wallet
            .try_broadcast_transaction(dlc.lock.0.clone())
            .await?;

        tracing::info!("Lock transaction published with txid {}", txid);

        // TODO: It's a bit suspicious to load this just to get the
        // refund timelock
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        self.monitor_actor
            .do_send_async(monitor::StartMonitoring {
                id: order_id,
                params: MonitorParams::from_dlc_and_timelocks(dlc, cfd.refund_timelock_in_blocks()),
            })
            .await?;

        Ok(())
    }

    async fn handle_monitoring_event(&mut self, event: monitor::Event) -> Result<()> {
        let order_id = event.order_id();

        let mut conn = self.db.acquire().await?;
        let cfd = load_cfd_by_order_id(order_id, &mut conn).await?;

        let new_state = cfd.handle(CfdStateChangeEvent::Monitor(event))?;

        insert_new_cfd_state_by_order_id(order_id, new_state.clone(), &mut conn).await?;

        // TODO: Not sure that should be done here...
        //  Consider bubbling the refund availability up to the user, and let user trigger
        //  transaction publication
        if let CfdState::MustRefund { .. } = new_state {
            let signed_refund_tx = cfd.refund_tx()?;
            let txid = self
                .wallet
                .try_broadcast_transaction(signed_refund_tx)
                .await?;

            tracing::info!("Refund transaction published on chain: {}", txid);
        }

        Ok(())
    }

    async fn handle_price_update(&mut self, quote: bitmex_price_feed::Quote) -> Result<()> {
        self.cfd_feed.set_current_price(quote);

        let mut conn = self.db.acquire().await?;
        self.cfd_feed.update(&mut conn).await?;
        Ok(())
    }
}

#[async_trait]
impl Handler<TakeOffer> for Actor {
    async fn handle(&mut self, msg: TakeOffer, _ctx: &mut Context<Self>) {
        log_error!(self.handle_take_offer(msg.order_id, msg.quantity));
    }
}

#[async_trait]
impl Handler<MakerStreamMessage> for Actor {
    async fn handle(
        &mut self,
        message: MakerStreamMessage,
        ctx: &mut Context<Self>,
    ) -> KeepRunning {
        let msg = match message.item {
            Ok(msg) => msg,
            Err(e) => {
                tracing::warn!("Error while receiving message from maker: {:#}", e);
                return KeepRunning::Yes;
            }
        };

        match msg {
            wire::MakerToTaker::CurrentOrder(current_order) => {
                log_error!(self.handle_new_order(current_order))
            }
            wire::MakerToTaker::ConfirmOrder(order_id) => {
                log_error!(self.handle_order_accepted(order_id, ctx))
            }
            wire::MakerToTaker::RejectOrder(order_id) => {
                log_error!(self.handle_order_rejected(order_id))
            }
            wire::MakerToTaker::InvalidOrderId(_) => todo!(),
            wire::MakerToTaker::Protocol(setup_msg) => {
                log_error!(self.handle_inc_protocol_msg(setup_msg))
            }
        }

        KeepRunning::Yes
    }
}

#[async_trait]
impl Handler<CfdSetupCompleted> for Actor {
    async fn handle(&mut self, msg: CfdSetupCompleted, _ctx: &mut Context<Self>) {
        log_error!(self.handle_cfd_setup_completed(msg.order_id, msg.dlc));
    }
}

#[async_trait]
impl Handler<monitor::Event> for Actor {
    async fn handle(&mut self, msg: monitor::Event, _ctx: &mut Context<Self>) {
        log_error!(self.handle_monitoring_event(msg))
    }
}

#[async_trait]
impl Handler<PriceUpdate> for Actor {
    async fn handle(&mut self, msg: PriceUpdate, _ctx: &mut Context<Self>) {
        log_error!(self.handle_price_update(msg.0))
    }
}

impl Message for TakeOffer {
    type Result = ();
}

// this signature is a bit different because we use `Address::attach_stream`
impl Message for MakerStreamMessage {
    type Result = KeepRunning;
}

impl Message for CfdSetupCompleted {
    type Result = ();
}

impl Message for PriceUpdate {
    type Result = ();
}

impl xtra::Actor for Actor {}
