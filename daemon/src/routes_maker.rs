use std::time::SystemTime;

use anyhow::Result;
use bdk::bitcoin::Amount;
use futures::stream::SelectAll;
use futures::StreamExt;
use rocket::fairing::AdHoc;
use rocket::figment::util::map;
use rocket::figment::value::{Map, Value};
use rocket::response::status;
use rocket::response::stream::{Event, EventStream};
use rocket::serde::json::Json;
use rocket::State;
use rocket_db_pools::{Connection, Database};
use rust_decimal_macros::dec;
use tokio::select;
use tokio::sync::{mpsc, watch};
use tokio_util::codec::{FramedRead, LengthDelimitedCodec};
use uuid::Uuid;

use crate::db;
use crate::model::cfd::{
    Cfd, CfdNewOfferRequest, CfdOffer, CfdState, CfdStateCommon, CfdTakeRequest,
};
use crate::model::Usd;
use crate::socket::*;
use crate::state::maker::{maker_do_something, Command};

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

#[get("/maker-feed")]
async fn maker_feed(
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

#[post("/offer/sell", data = "<cfd_confirm_offer_request>")]
async fn post_sell_offer(
    cfd_confirm_offer_request: Json<CfdNewOfferRequest>,
    queue: &State<mpsc::Sender<CfdOffer>>,
) -> Result<status::Accepted<()>, status::BadRequest<String>> {
    let offer = CfdOffer::from_default_with_price(cfd_confirm_offer_request.price)
        .map_err(|e| status::BadRequest(Some(e.to_string())))?
        .with_min_quantity(cfd_confirm_offer_request.min_quantity)
        .with_max_quantity(cfd_confirm_offer_request.max_quantity);

    let _res = queue
        .send(offer)
        .await
        .map_err(|_| status::BadRequest(Some("internal server error".to_string())))?;

    Ok(status::Accepted(None))
}

// TODO: Shall we use a simpler struct for verification? AFAICT quantity is not
// needed, no need to send the whole CFD either as the other fields can be generated from the offer
#[post("/offer/confirm", data = "<cfd_confirm_offer_request>")]
async fn post_confirm_offer(
    cfd_confirm_offer_request: Json<CfdTakeRequest>,
    queue: &State<mpsc::Sender<CfdOffer>>,
    mut conn: Connection<db::maker::Maker>,
) -> Result<status::Accepted<()>, status::BadRequest<String>> {
    dbg!(&cfd_confirm_offer_request);

    let offer = db::load_offer_by_id_from_conn(cfd_confirm_offer_request.offer_id, &mut conn)
        .await
        .map_err(|e| status::BadRequest(Some(e.to_string())))?;

    let _res = queue
        .send(offer)
        .await
        .map_err(|_| status::BadRequest(Some("internal server error".to_string())))?;

    Ok(status::Accepted(None))
}

#[get("/alive")]
fn get_health_check() {}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct RetrieveCurrentOffer;

pub async fn start_http() -> Result<()> {
    let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
    let (offer_feed_sender, offer_feed_receiver) = watch::channel::<Option<CfdOffer>>(None);
    let (_balance_feed_sender, balance_feed_receiver) = watch::channel::<Amount>(Amount::ONE_BTC);

    let (new_cfd_offer_sender, mut new_cfd_offer_receiver) = mpsc::channel::<CfdOffer>(1024);

    let (db_command_sender, db_command_receiver) = mpsc::channel::<Command>(1024);

    // init the CFD feed, this will be picked up by the receiver managed by rocket once started
    db_command_sender
        .send(Command::RefreshCfdFeed)
        .await
        .unwrap();

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9999").await?;

    let local_addr = listener.local_addr().unwrap();
    println!("Listening on {}", local_addr);

    tokio::spawn({
        let db_command_sender = db_command_sender.clone();
        let offer_feed_receiver = offer_feed_receiver.clone();
        async move {
            let mut read_connections = SelectAll::new();
            let mut write_connections = std::collections::HashMap::new();
            loop {
                select! {
                    Ok((socket, remote_addr)) = listener.accept() => {
                        println!("Connected to {}", remote_addr);
                        let uuid = Uuid::new_v4();
                        let (read, write) = socket.into_split();

                        let messages = FramedRead::new(read, LengthDelimitedCodec::new())
                            .map(|result| {
                                let message = serde_json::from_slice::<Message>(&result?)?;
                                anyhow::Result::<_>::Ok(message)
                            })
                            .map(move |message_result| (uuid, remote_addr, message_result));

                        read_connections.push(messages);
                        let sender = spawn_sender(write);

                        if let Some(latest_offer) = &*offer_feed_receiver.borrow() {
                            sender.send(Message::CurrentOffer(Some(latest_offer.clone()))).expect("Could not communicate with taker");
                        }

                        write_connections.insert(uuid, sender);
                    },

                    Some(cfd_offer) = new_cfd_offer_receiver.recv() => {
                        db_command_sender.send(Command::SaveOffer(cfd_offer.clone())).await.unwrap();

                        offer_feed_sender.send(Some(cfd_offer.clone())).unwrap();

                        for sender in write_connections.values() {
                            sender.send(Message::CurrentOffer(Some(cfd_offer.clone()))).expect("Could not communicate with taker");
                        }

                    },
                    Some((uuid, _peer, message)) = read_connections.next() => {
                        match message {
                            Ok(Message::TakeOffer(cfd_take_request)) => {
                                println!("Received a CFD offer take request {:?}", cfd_take_request);

                                if offer_feed_receiver.borrow().as_ref().is_none() {
                                   eprintln!("Maker has no current offer anymore - can't handle a take request");
                                   return;
                                }

                                let current_offer = offer_feed_receiver.borrow().as_ref().unwrap().clone();
                                assert_eq!(current_offer.id, cfd_take_request.offer_id, "You can only confirm the current offer");

                                    let cfd = Cfd::new(current_offer, cfd_take_request.quantity, CfdState::PendingTakeRequest{
                                        common: CfdStateCommon {
                                            transition_timestamp: SystemTime::now()
                                        },
                                    },
                                    Usd(dec!(10001))).unwrap();

                                db_command_sender.send(Command::SaveCfd(cfd.clone())).await.unwrap();

                                // FIXME: Use a button on the CFD tile to
                                // confirm instead of auto-confirmation
                                write_connections.get(&uuid).expect("taker to still online")
                                    .send(Message::ConfirmTakeOffer(cfd_take_request.offer_id)).unwrap();

                                db_command_sender.send(Command::SaveNewCfdStateByOfferId(cfd.offer_id, CfdState::Accepted { common: CfdStateCommon { transition_timestamp: SystemTime::now()}})).await.unwrap();

                                // Remove the current offer as it got accepted
                                offer_feed_sender.send(None).unwrap();

                                for sender in write_connections.values() {
                                    sender.send(Message::CurrentOffer(None)).expect("Could not communicate with taker");
                                }
                            },
                            Ok(Message::StartContractSetup(offer_id)) => {
                                db_command_sender.send(Command::SaveNewCfdStateByOfferId(offer_id, CfdState::ContractSetup { common: CfdStateCommon { transition_timestamp: SystemTime::now()}})).await.unwrap();
                                db_command_sender.send(Command::RefreshCfdFeed).await.unwrap();
                            }
                            Ok(Message::CurrentOffer(_)) => {
                                panic!("Maker should not receive current offer");
                            },
                            Ok(Message::ConfirmTakeOffer(_)) => {
                                panic!("Maker should not receive offer confirmations");
                            },
                            Err(error) => {
                                eprintln!("Error in reading message: {}", error );
                            }
                        }
                    }
                }
            }
        }
    });

    let db: Map<_, Value> = map! {
        "url" => "./maker.sqlite".into(),
    };

    let figment = rocket::Config::figment()
        .merge(("databases", map!["maker" => db]))
        .merge(("port", 8001));

    rocket::custom(figment)
        .manage(cfd_feed_receiver)
        .manage(offer_feed_receiver)
        .manage(new_cfd_offer_sender)
        .manage(balance_feed_receiver)
        .manage(db_command_sender)
        .attach(db::maker::Maker::init())
        .attach(AdHoc::try_on_ignite(
            "SQL migrations",
            db::maker::run_migrations,
        ))
        .attach(AdHoc::try_on_ignite("send command to the db", |rocket| {
            maker_do_something(rocket, db_command_receiver, cfd_feed_sender)
        }))
        .mount(
            "/",
            routes![
                maker_feed,
                post_sell_offer,
                post_confirm_offer,
                get_health_check
            ],
        )
        .launch()
        .await?;

    Ok(())
}
