use crate::db::{
    insert_cfd, insert_cfd_offer, insert_new_cfd_state, insert_new_cfd_state_by_offer_id,
    load_all_cfds, load_offer_by_id,
};
use crate::model::cfd::{Cfd, CfdOffer, CfdOfferId, CfdState};
use tokio::sync::watch;

pub mod maker;
pub mod taker;

#[derive(Debug)]
pub enum Command {
    SaveOffer(CfdOffer),
    SaveCfd(Cfd),
    SaveNewCfdState(Cfd),
    SaveNewCfdStateByOfferId(CfdOfferId, CfdState),
    RefreshCfdFeed,
}

pub async fn handle_command(
    db: &sqlx::SqlitePool,
    command: Command,
    cfd_feed_sender: &watch::Sender<Vec<Cfd>>,
) -> anyhow::Result<()> {
    println!("Handle command: {:?}", command);

    match command {
        Command::SaveOffer(cfd_offer) => {
            // Only save offer when it wasn't already saved (e.g. taker
            // can see the same "latest" offer when it comes back online)
            if let Ok(offer) = load_offer_by_id(cfd_offer.id, db).await {
                println!("Offer with id {:?} already stored in the db.", offer.id);
            } else {
                insert_cfd_offer(cfd_offer, db).await?
            }
        }
        Command::SaveCfd(cfd) => {
            insert_cfd(cfd, db).await?;
            cfd_feed_sender.send(load_all_cfds(db).await?)?;
        }
        Command::SaveNewCfdState(cfd) => {
            insert_new_cfd_state(cfd, db).await?;
            cfd_feed_sender.send(load_all_cfds(db).await?)?;
        }

        Command::SaveNewCfdStateByOfferId(offer_id, state) => {
            insert_new_cfd_state_by_offer_id(offer_id, state, db).await?;
            cfd_feed_sender.send(load_all_cfds(db).await?)?;
        }
        Command::RefreshCfdFeed => {
            cfd_feed_sender.send(load_all_cfds(db).await?)?;
        }
    }

    Ok(())
}
