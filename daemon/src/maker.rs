use anyhow::Result;
use bdk::bitcoin::Amount;
use model::cfd::{Cfd, CfdOffer};
use rocket::fairing::AdHoc;
use rocket::figment::util::map;
use rocket::figment::value::{Map, Value};
use rocket_db_pools::Database;
use tokio::sync::{mpsc, watch};

mod db;
mod maker_cfd_actor;
mod maker_inc_connections_actor;
mod model;
mod routes_maker;
mod send_wire_message_actor;
mod to_sse_event;
mod wire;

#[derive(Database)]
#[database("maker")]
pub struct Db(sqlx::SqlitePool);

#[rocket::main]
async fn main() -> Result<()> {
    let (_cfd_feed_sender, cfd_feed_receiver) = watch::channel::<Vec<Cfd>>(vec![]);
    let (_offer_feed_sender, offer_feed_receiver) = watch::channel::<Option<CfdOffer>>(None);
    let (_balance_feed_sender, balance_feed_receiver) = watch::channel::<Amount>(Amount::ONE_BTC);

    let db: Map<_, Value> = map! {
        "url" => "./maker.sqlite".into(),
    };

    let figment = rocket::Config::figment()
        .merge(("databases", map!["maker" => db]))
        .merge(("port", 8001));

    let listener = tokio::net::TcpListener::bind("0.0.0.0:9999").await?;
    let local_addr = listener.local_addr().unwrap();

    println!("Listening on {}", local_addr);

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

            let (connections_actor_inbox_sender, connections_actor_inbox_recv) =
                mpsc::unbounded_channel();

            let (cfd_maker_actor, cfd_maker_actor_inbox) =
                maker_cfd_actor::new(db, connections_actor_inbox_sender);
            let connections_actor = maker_inc_connections_actor::new(
                listener,
                cfd_maker_actor_inbox.clone(),
                connections_actor_inbox_recv,
            );

            tokio::spawn(cfd_maker_actor);
            tokio::spawn(connections_actor);

            Ok(rocket.manage(cfd_maker_actor_inbox))
        }))
        .mount(
            "/",
            rocket::routes![
                routes_maker::maker_feed,
                routes_maker::post_sell_offer,
                // routes_maker::post_confirm_offer,
                routes_maker::get_health_check
            ],
        )
        .launch()
        .await?;

    Ok(())
}
