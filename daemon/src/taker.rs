use anyhow::Result;
use bdk::bitcoin::Amount;
use model::cfd::{Cfd, CfdOffer};
use rocket::fairing::AdHoc;
use rocket::figment::util::map;
use rocket::figment::value::{Map, Value};
use rocket_db_pools::Database;
use tokio::sync::watch;

mod db;
mod model;
mod routes_taker;
mod send_wire_message_actor;
mod taker_cfd_actor;
mod taker_inc_message_actor;
mod to_sse_event;
mod wire;

#[derive(Database)]
#[database("taker")]
pub struct Db(sqlx::SqlitePool);

#[rocket::main]
async fn main() -> Result<()> {
    let (cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
    let (offer_feed_sender, offer_feed_receiver) = watch::channel::<Option<CfdOffer>>(None);
    let (_balance_feed_sender, balance_feed_receiver) = watch::channel::<Amount>(Amount::ONE_BTC);

    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    let connection = socket
        .connect("0.0.0.0:9999".parse().unwrap())
        .await
        .expect("Maker should be online first");

    let (read, write) = connection.into_split();

    let db: Map<_, Value> = map! {
        "url" => "./taker.sqlite".into(),
    };

    let figment = rocket::Config::figment().merge(("databases", map!["taker" => db]));

    rocket::custom(figment)
        .manage(cfd_feed_receiver)
        .manage(offer_feed_receiver)
        .manage(balance_feed_receiver)
        .attach(Db::init())
        .attach(AdHoc::try_on_ignite(
            "SQL migrations",
            |rocket| async move {
                match Db::fetch(&rocket) {
                    Some(db) => match db::run_migrations(&**db).await {
                        Ok(_) => Ok(rocket),
                        Err(_) => Err(rocket),
                    },
                    None => Err(rocket),
                }
            },
        ))
        .attach(AdHoc::try_on_ignite("Create actors", |rocket| async move {
            let db = match Db::fetch(&rocket) {
                Some(db) => (**db).clone(),
                None => return Err(rocket),
            };

            let (out_maker_messages_actor, out_maker_actor_inbox) =
                send_wire_message_actor::new(write);
            let (cfd_actor, cfd_actor_inbox) = taker_cfd_actor::new(
                db,
                cfd_feed_sender,
                offer_feed_sender,
                out_maker_actor_inbox,
            );
            let inc_maker_messages_actor =
                taker_inc_message_actor::new(read, cfd_actor_inbox.clone());

            tokio::spawn(cfd_actor);
            tokio::spawn(inc_maker_messages_actor);
            tokio::spawn(out_maker_messages_actor);

            Ok(rocket.manage(cfd_actor_inbox))
        }))
        .mount(
            "/",
            rocket::routes![
                routes_taker::feed,
                routes_taker::post_cfd,
                routes_taker::get_health_check
            ],
        )
        .launch()
        .await?;

    Ok(())
}
