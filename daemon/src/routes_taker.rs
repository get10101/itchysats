use std::time::SystemTime;

use anyhow::Result;
use bdk::bitcoin::Amount;
use futures::StreamExt;
use rocket::fairing::AdHoc;
use rocket::figment::util::map;
use rocket::figment::value::{Map, Value};
use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::Json;
use rocket::State;
use rocket_db_pools::{Connection, Database};
use rust_decimal::Decimal;
use tokio::select;
use tokio::sync::{mpsc, watch};
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};

use crate::db;
use crate::model::cfd::{Cfd, CfdOffer, CfdState, CfdStateCommon, CfdTakeRequest};
use crate::model::{Position, Usd};
use crate::socket::*;
use crate::state::taker::{hey_db_do_something, Command};

trait ToSseEvent {
    fn to_sse_event(&self) -> Event;
}

impl ToSseEvent for Vec<Cfd> {
    fn to_sse_event(&self) -> Event {
        Event::json(self).event("cfds")
    }
}

impl ToSseEvent for Option<CfdOffer> {
    fn to_sse_event(&self) -> Event {
        Event::json(self).event("offer")
    }
}

impl ToSseEvent for Amount {
    fn to_sse_event(&self) -> Event {
        Event::json(&self.as_btc()).event("balance")
    }
}

#[get("/feed")]
async fn feed(
    rx_cfds: &State<watch::Receiver<Vec<Cfd>>>,
    rx_offer: &State<watch::Receiver<Option<CfdOffer>>>,
    rx_balance: &State<watch::Receiver<Amount>>,
) -> EventStream![] {
    let mut rx_cfds = rx_cfds.inner().clone();
    let mut rx_offer = rx_offer.inner().clone();
    let mut rx_balance = rx_balance.inner().clone();

    EventStream! {
        let balance = rx_balance.borrow().clone();
        yield balance.to_sse_event();

        let offer = rx_offer.borrow().clone();
        yield offer.to_sse_event();

        let cfds = rx_cfds.borrow().clone();
        yield cfds.to_sse_event();

        loop{
            select! {
                Ok(()) = rx_balance.changed() => {
                    let balance = rx_balance.borrow().clone();
                    yield balance.to_sse_event();
                },
                Ok(()) = rx_offer.changed() => {
                    let offer = rx_offer.borrow().clone();
                    yield offer.to_sse_event();
                }
                Ok(()) = rx_cfds.changed() => {
                    let cfds = rx_cfds.borrow().clone();
                    yield cfds.to_sse_event();
                }
            }
        }
    }
}

#[post("/cfd", data = "<cfd_take_request>")]
async fn post_cfd(
    cfd_take_request: Json<CfdTakeRequest>,
    cfd_sender: &State<mpsc::Sender<Cfd>>,
    db_command_sender: &State<mpsc::Sender<Command>>,
    mut conn: Connection<db::taker::Taker>,
) {
    let current_offer = db::load_offer_by_id_from_conn(cfd_take_request.offer_id, &mut conn)
        .await
        .unwrap();

    println!("Accepting current offer: {:?}", &current_offer);

    let cfd = Cfd {
        offer_id: current_offer.id,
        initial_price: current_offer.price,
        leverage: current_offer.leverage,
        trading_pair: current_offer.trading_pair,
        liquidation_price: current_offer.liquidation_price,
        position: Position::Buy,
        quantity_usd: cfd_take_request.quantity,
        profit_btc: Amount::ZERO,
        profit_usd: Usd(Decimal::ZERO),
        state: CfdState::TakeRequested {
            common: CfdStateCommon {
                transition_timestamp: SystemTime::now(),
            },
        },
    };

    db_command_sender
        .send(Command::SaveCfd(cfd.clone()))
        .await
        .unwrap();

    // TODO: remove unwrap
    cfd_sender.send(cfd).await.unwrap();
}

#[get("/alive")]
fn get_health_check() {}

pub async fn start_http() -> Result<()> {
    let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);

    let (offer_feed_sender, offer_feed_receiver) = watch::channel::<Option<CfdOffer>>(None);
    let (_balance_feed_sender, balance_feed_receiver) = watch::channel::<Amount>(Amount::ONE_BTC);

    let (take_cfd_sender, mut take_cfd_receiver) = mpsc::channel::<Cfd>(1024);

    let (db_command_sender, db_command_receiver) = mpsc::channel::<Command>(1024);

    // init the CFD feed, this will be picked up by the receiver managed by rocket once started
    db_command_sender
        .send(Command::RefreshCfdFeed)
        .await
        .unwrap();

    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    let connection = socket
        .connect("0.0.0.0:9999".parse().unwrap())
        .await
        .expect("Maker should be online first");

    let (read, write) = connection.into_split();

    tokio::spawn({
        let db_command_sender = db_command_sender.clone();
        let mut cfd_feed_receiver = cfd_feed_receiver.clone();

        async move {
            let frame_read = FramedRead::new(read, LengthDelimitedCodec::new());

            let mut messages = frame_read.map(|result| {
                let message = serde_json::from_slice::<Message>(&result?)?;
                anyhow::Result::<_>::Ok(message)
            });

            let sender = spawn_sender(write);

            loop {
                select! {
                    Some(cfd) = take_cfd_receiver.recv() => {
                        sender.send(Message::TakeOffer(CfdTakeRequest { offer_id : cfd.offer_id, quantity : cfd.quantity_usd})).unwrap();

                        let cfd_with_new_state = Cfd {
                            state : CfdState::PendingTakeRequest {
                                common: CfdStateCommon {
                                    transition_timestamp: SystemTime::now(),
                                },
                            },
                            ..cfd
                        };
                        db_command_sender.send(Command::SaveNewCfdState(cfd_with_new_state)).await.unwrap();
                    },
                    Some(message) = messages.next() => {
                        match message {
                            Ok(Message::TakeOffer(_)) => {
                                eprintln!("Taker should not receive take requests");
                            },
                            Ok(Message::CurrentOffer(offer)) => {
                                if let Some(offer) = &offer {
                                    println!("Received new offer from the maker: {:?}", offer );
                                    db_command_sender.send(Command::SaveOffer(offer.clone())).await.unwrap();
                                }
                                else {
                                    println!("Maker does not have an offer anymore");
                                }
                                offer_feed_sender.send(offer).unwrap();
                            },
                            Ok(Message::StartContractSetup(_)) => {
                                eprintln!("Taker should not receive start contract setup message as the taker sends it");
                            }
                            Ok(Message::ConfirmTakeOffer(offer_id)) => {
                                println!("The maker has accepted your take request for offer: {:?}", offer_id );
                                let new_state : CfdState=
                                     CfdState::Accepted {
                                        common: CfdStateCommon {
                                            transition_timestamp: SystemTime::now(),
                                        },
                                };

                                db_command_sender.send(Command::SaveNewCfdStateByOfferId(offer_id, new_state)).await.unwrap();
                            },
                            Err(error) => {
                                eprintln!("Error in reading message: {}", error );
                            }
                        }
                    },
                    Ok(()) = cfd_feed_receiver.changed() => {
                        let cfds = cfd_feed_receiver.borrow().clone();

                        let to_be_accepted = cfds.into_iter().filter(|by| matches!(by.state, CfdState::Accepted {..})).collect::<Vec<Cfd>>();

                        for mut cfd in to_be_accepted {
                            let new_state : CfdState=
                                     CfdState::ContractSetup {
                                        common: CfdStateCommon {
                                            transition_timestamp: SystemTime::now(),
                                        },
                                };

                            cfd.state = new_state;

                            db_command_sender.send(Command::SaveNewCfdState(cfd)).await.unwrap();

                            // TODO: Send message to Maker for contract setup (and transition Maker into that state upon receiving the message)
                        }
                    }
                }
            }
        }
    });

    let db: Map<_, Value> = map! {
        "url" => "./taker.sqlite".into(),
    };

    let figment = rocket::Config::figment().merge(("databases", map!["taker" => db]));

    rocket::custom(figment)
        .manage(offer_feed_receiver)
        .manage(cfd_feed_receiver)
        .manage(take_cfd_sender)
        .manage(balance_feed_receiver)
        .manage(db_command_sender)
        .attach(db::taker::Taker::init())
        .attach(AdHoc::try_on_ignite(
            "SQL migrations",
            db::taker::run_migrations,
        ))
        .attach(AdHoc::try_on_ignite("send command to the db", |rocket| {
            hey_db_do_something(rocket, db_command_receiver, cfd_feed_sender)
        }))
        .mount("/", routes![feed, post_cfd, get_health_check])
        .launch()
        .await?;

    Ok(())
}
