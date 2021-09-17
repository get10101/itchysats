use std::time::SystemTime;

use crate::db::{insert_cfd, insert_order, load_all_cfds, load_order_by_id, Origin};
use crate::model::cfd::{Cfd, CfdState, CfdStateCommon, FinalizedCfd, Order, OrderId};
use crate::model::{TakerId, Usd};
use crate::wire::SetupMsg;
use crate::{maker_cfd_actor, maker_inc_connections_actor, setup_contract_actor};
use bdk::bitcoin::secp256k1::schnorrsig;
use bdk::bitcoin::{self};
use bdk::database::BatchDatabase;
use cfd_protocol::WalletExt;
use futures::Future;
use tokio::sync::{mpsc, watch};

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Command {
    TakeOrder {
        taker_id: TakerId,
        order_id: OrderId,
        quantity: Usd,
    },
    NewOrder(Order),
    StartContractSetup {
        taker_id: TakerId,
        order_id: OrderId,
    },
    NewTakerOnline {
        id: TakerId,
    },
    IncProtocolMsg(SetupMsg),
    CfdSetupCompleted(FinalizedCfd),
}

pub fn new<B, D>(
    db: sqlx::SqlitePool,
    wallet: bdk::Wallet<B, D>,
    oracle_pk: schnorrsig::PublicKey,
    takers: mpsc::UnboundedSender<maker_inc_connections_actor::Command>,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    order_feed_sender: watch::Sender<Option<Order>>,
) -> (
    impl Future<Output = ()>,
    mpsc::UnboundedSender<maker_cfd_actor::Command>,
)
where
    D: BatchDatabase,
{
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let mut current_contract_setup = None;

    let mut current_order_id = None;

    let actor = {
        let sender = sender.clone();

        async move {
            // populate the CFD feed with existing CFDs
            let mut conn = db.acquire().await.unwrap();
            cfd_feed_actor_inbox
                .send(load_all_cfds(&mut conn).await.unwrap())
                .unwrap();

            while let Some(message) = receiver.recv().await {
                match message {
                    maker_cfd_actor::Command::TakeOrder {
                        taker_id,
                        order_id,
                        quantity,
                    } => {
                        println!(
                            "Taker {} wants to take {} of order {}",
                            taker_id, quantity, order_id
                        );

                        let mut conn = db.acquire().await.unwrap();

                        // 1. Validate if order is still valid
                        let current_order = match current_order_id {
                            Some(current_order_id) if current_order_id == order_id => {
                                load_order_by_id(current_order_id, &mut conn).await.unwrap()
                            }
                            _ => {
                                takers
                                .send(maker_inc_connections_actor::Command::NotifyInvalidOrderId {
                                    id: order_id,
                                    taker_id,
                                })
                                .unwrap();
                                continue;
                            }
                        };

                        // 2. Insert CFD in DB
                        // TODO: Don't auto-accept, present to user in UI instead
                        let cfd = Cfd::new(
                            current_order.clone(),
                            quantity,
                            CfdState::Accepted {
                                common: CfdStateCommon {
                                    transition_timestamp: SystemTime::now(),
                                },
                            },
                            current_order.position,
                        );
                        insert_cfd(cfd, &mut conn).await.unwrap();

                        takers
                            .send(maker_inc_connections_actor::Command::NotifyOrderAccepted {
                                id: order_id,
                                taker_id,
                            })
                            .unwrap();
                        cfd_feed_actor_inbox
                            .send(load_all_cfds(&mut conn).await.unwrap())
                            .unwrap();

                        // 3. Remove current order
                        current_order_id = None;
                        takers
                            .send(maker_inc_connections_actor::Command::BroadcastOrder(None))
                            .unwrap();
                        order_feed_sender.send(None).unwrap();
                    }
                    maker_cfd_actor::Command::NewOrder(order) => {
                        // 1. Save to DB
                        let mut conn = db.acquire().await.unwrap();
                        insert_order(&order, &mut conn, Origin::Ours).await.unwrap();

                        // 2. Update actor state to current order
                        current_order_id.replace(order.id);

                        // 3. Notify UI via feed
                        order_feed_sender.send(Some(order.clone())).unwrap();

                        // 4. Inform connected takers
                        takers
                            .send(maker_inc_connections_actor::Command::BroadcastOrder(Some(
                                order,
                            )))
                            .unwrap();
                    }
                    maker_cfd_actor::Command::NewTakerOnline { id: taker_id } => {
                        let mut conn = db.acquire().await.unwrap();

                        let current_order = match current_order_id {
                            Some(current_order_id) => {
                                Some(load_order_by_id(current_order_id, &mut conn).await.unwrap())
                            }
                            None => None,
                        };

                        takers
                            .send(maker_inc_connections_actor::Command::SendOrder {
                                order: current_order,
                                taker_id,
                            })
                            .unwrap();
                    }
                    maker_cfd_actor::Command::StartContractSetup { taker_id, order_id } => {
                        // Kick-off the CFD protocol
                        let (sk, pk) = crate::keypair::new(&mut rand::thread_rng());

                        // TODO: Load correct quantity from DB with order_id
                        let maker_params = wallet
                            .build_party_params(bitcoin::Amount::ZERO, pk)
                            .unwrap();

                        let cfd = load_order_by_id(order_id, &mut conn).await.unwrap();

                        let (actor, inbox) = setup_contract_actor::new(
                            {
                                let inbox = takers.clone();
                                move |msg| {
                                    inbox
                                        .send(
                                            maker_inc_connections_actor::Command::OutProtocolMsg {
                                                taker_id,
                                                msg,
                                            },
                                        )
                                        .unwrap()
                                }
                            },
                            setup_contract_actor::OwnParams::Maker(maker_params),
                            sk,
                            oracle_pk,
                            cfd,
                        );

                        tokio::spawn({
                            let sender = sender.clone();

                            async move {
                                sender
                                    .send(Command::CfdSetupCompleted(actor.await))
                                    .unwrap()
                            }
                        });
                        current_contract_setup = Some(inbox);
                    }
                    maker_cfd_actor::Command::IncProtocolMsg(msg) => {
                        let inbox = match &current_contract_setup {
                            None => panic!("whoops"),
                            Some(inbox) => inbox,
                        };

                        inbox.send(msg).unwrap();
                    }
                    maker_cfd_actor::Command::CfdSetupCompleted(_finalized_cfd) => {
                        todo!("but what?")
                    }
                }
            }
        }
    };

    (actor, sender)
}
