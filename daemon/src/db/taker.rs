use crate::db::do_run_migrations;
use rocket::{fairing, Build, Rocket};
use rocket_db_pools::{sqlx, Database};

#[derive(Database)]
#[database("taker")]
pub struct Taker(sqlx::SqlitePool);

pub async fn run_migrations(rocket: Rocket<Build>) -> fairing::Result {
    match Taker::fetch(&rocket) {
        Some(db) => match do_run_migrations(&**db).await {
            Ok(_) => Ok(rocket),
            Err(_) => Err(rocket),
        },
        None => Err(rocket),
    }
}
