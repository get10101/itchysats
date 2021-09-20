use crate::db::{
    insert_cfd, insert_new_cfd_state_by_order_id, insert_order, load_all_cfds,
    load_cfd_by_order_id, load_order_by_id,
};
use crate::model::cfd::{Cfd, CfdState, CfdStateCommon, Dlc, Order, OrderId};
use crate::model::{TakerId, Usd, WalletInfo};
use crate::wallet::Wallet;
use crate::wire::SetupMsg;
use crate::{maker_cfd_actor, maker_inc_connections_actor, setup_contract_actor};
use bdk::bitcoin::secp256k1::schnorrsig;
use futures::Future;
use std::time::SystemTime;
use tokio::sync::{mpsc, watch};

#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum Command {
    SyncWallet,
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
    CfdSetupCompleted {
        order_id: OrderId,
        dlc: Dlc,
    },
}

pub fn new(
    db: sqlx::SqlitePool,
    wallet: Wallet,
    oracle_pk: schnorrsig::PublicKey,
    takers: mpsc::UnboundedSender<maker_inc_connections_actor::Command>,
    cfd_feed_actor_inbox: watch::Sender<Vec<Cfd>>,
    order_feed_sender: watch::Sender<Option<Order>>,
    wallet_feed_sender: watch::Sender<WalletInfo>,
) -> (
    impl Future<Output = ()>,
    mpsc::UnboundedSender<maker_cfd_actor::Command>,
) {
    let (sender, mut receiver) = mpsc::unbounded_channel();

    // TODO: Move the contract setup into a dedicated actor and send messages to that actor that
    // manages the state instead of this ugly buffer
    let mut current_contract_setup = None;
    let mut contract_setup_message_buffer = vec![];

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
                    maker_cfd_actor::Command::SyncWallet => {
                        let wallet_info = wallet.sync().await.unwrap();
                        wallet_feed_sender.send(wallet_info).unwrap();
                    }
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
                        insert_order(&order, &mut conn).await.unwrap();

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
                        println!("CONTRACT SETUP");

                        // Kick-off the CFD protocol
                        let (sk, pk) = crate::keypair::new(&mut rand::thread_rng());

                        let cfd = load_cfd_by_order_id(order_id, &mut conn).await.unwrap();
                        let margin = cfd.margin().unwrap();

                        let maker_params = wallet.build_party_params(margin, pk).await.unwrap();

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
                            wallet.clone(),
                        );

                        current_contract_setup = Some(inbox.clone());

                        for msg in contract_setup_message_buffer.drain(..) {
                            inbox.send(msg).unwrap();
                        }

                        // TODO: Should we do this here or already earlier or after the spawn?
                        insert_new_cfd_state_by_order_id(
                            order_id,
                            CfdState::ContractSetup {
                                common: CfdStateCommon {
                                    transition_timestamp: SystemTime::now(),
                                },
                            },
                            &mut conn,
                        )
                        .await
                        .unwrap();
                        cfd_feed_actor_inbox
                            .send(load_all_cfds(&mut conn).await.unwrap())
                            .unwrap();

                        tokio::spawn({
                            let sender = sender.clone();

                            async move {
                                sender
                                    .send(Command::CfdSetupCompleted {
                                        order_id,
                                        dlc: actor.await,
                                    })
                                    .unwrap()
                            }
                        });
                    }
                    maker_cfd_actor::Command::IncProtocolMsg(msg) => {
                        let inbox = match &current_contract_setup {
                            None => {
                                contract_setup_message_buffer.push(msg);
                                continue;
                            }
                            Some(inbox) => inbox,
                        };

                        inbox.send(msg).unwrap();
                    }
                    maker_cfd_actor::Command::CfdSetupCompleted { order_id, dlc } => {
                        println!("Setup complete, publishing on chain now...");

                        current_contract_setup = None;
                        contract_setup_message_buffer = vec![];

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
                        .await
                        .unwrap();

                        cfd_feed_actor_inbox
                            .send(load_all_cfds(&mut conn).await.unwrap())
                            .unwrap();

                        let txid = wallet.try_broadcast_transaction(dlc.lock).await.unwrap();

                        println!("Lock transaction published with txid {}", txid);

                        // TODO: tx monitoring, once confirmed with x blocks transition the Cfd to
                        // Open
                    }
                }
            }
        }
    };

    (actor, sender)
}
