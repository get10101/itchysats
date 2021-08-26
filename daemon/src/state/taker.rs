use crate::model::cfd::{Cfd, CfdOffer, CfdOfferId, CfdState};
use crate::state;
use crate::state::handle_command;
use rocket::{fairing, Build, Rocket};
use rocket_db_pools::Database;
use std::convert::{TryFrom, TryInto};
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone)]
pub enum Command {
    SaveOffer(CfdOffer),
    SaveCfd(Cfd),
    SaveNewCfdState(Cfd),
    SaveNewCfdStateByOfferId(CfdOfferId, CfdState),
    /// All other commands that are interacting with Cfds perform a refresh
    /// automatically - as such, RefreshCfdFeed should be used only on init
    RefreshCfdFeed,
}

pub struct CommandError;

impl TryFrom<Command> for state::Command {
    type Error = CommandError;

    fn try_from(value: Command) -> Result<Self, Self::Error> {
        let command = match value {
            Command::SaveOffer(offer) => state::Command::SaveOffer(offer),
            Command::SaveCfd(cfd) => state::Command::SaveCfd(cfd),
            Command::SaveNewCfdState(cfd) => state::Command::SaveNewCfdState(cfd),
            Command::SaveNewCfdStateByOfferId(uuid, cfd_state) => {
                state::Command::SaveNewCfdStateByOfferId(uuid, cfd_state)
            }
            Command::RefreshCfdFeed => state::Command::RefreshCfdFeed,
        };

        Ok(command)
    }
}

pub async fn hey_db_do_something(
    rocket: Rocket<Build>,
    mut db_command_receiver: mpsc::Receiver<Command>,
    cfd_feed_sender: watch::Sender<Vec<Cfd>>,
) -> fairing::Result {
    let db = match crate::db::taker::Taker::fetch(&rocket) {
        Some(db) => (**db).clone(),
        None => return Err(rocket),
    };

    tokio::spawn(async move {
        while let Some(command) = db_command_receiver.recv().await {
            match command.clone().try_into() {
                Ok(shared_command) => {
                    handle_command(&db, shared_command, &cfd_feed_sender)
                        .await
                        .unwrap();
                }
                Err(_) => unreachable!("currently there are only shared commands"),
            }
        }
    });

    Ok(rocket)
}
