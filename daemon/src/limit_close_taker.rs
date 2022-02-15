use crate::db::delete_limit_close_price;
use crate::db::insert_or_update_limit_close_price;
use crate::db::load_all_cfd_ids;
use crate::db::load_cfd;
use crate::db::load_limit_close_price;
use crate::taker_cfd;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use model::cfd::OrderId;
use model::Position;
use model::Price;
use std::time::Duration;
use tokio_tasks::Tasks;
use xtra::Address;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

pub struct Actor<O, W, P> {
    db: sqlx::SqlitePool,
    taker_cfd_actor: Address<taker_cfd::Actor<O, W>>,
    price_feed_actor: Address<P>,
    tasks: Tasks,
}

impl<O, W, P> Actor<O, W, P> {
    pub fn new(
        db: sqlx::SqlitePool,
        cfd: Address<taker_cfd::Actor<O, W>>,
        price_feed_actor: Address<P>,
    ) -> Self {
        Self {
            db,
            taker_cfd_actor: cfd,
            price_feed_actor,
            tasks: Tasks::default(),
        }
    }
}

#[xtra_productivity]
impl<O, W, P> Actor<O, W, P>
where
    O: xtra::Handler<taker_cfd::ProposeSettlement>,
    W: xtra::Handler<taker_cfd::ProposeSettlement>,
    P: xtra::Handler<xtra_bitmex_price_feed::LatestQuote>,
{
    async fn handle_set_limit_close_price(&mut self, msg: SetLimitClosePrice) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        match msg.price {
            None => {
                delete_limit_close_price(msg.id, &mut conn).await?;
            }
            Some(price) => {
                insert_or_update_limit_close_price(msg.id, price, &mut conn).await?;
            }
        }

        Ok(())
    }
}

#[xtra_productivity]
impl<O, W, P> Actor<O, W, P>
where
    O: xtra::Handler<taker_cfd::ProposeSettlement>,
    W: xtra::Handler<taker_cfd::ProposeSettlement>,
    P: xtra::Handler<xtra_bitmex_price_feed::LatestQuote>,
{
    async fn handle_sync(&mut self, _msg: Sync) {
        if let Err(e) = self.limit_close().await {
            tracing::error!("Limit close failed: {e:#}");
        }
    }
}

impl<O, W, P> Actor<O, W, P>
where
    O: xtra::Handler<taker_cfd::ProposeSettlement>,
    W: xtra::Handler<taker_cfd::ProposeSettlement>,
    P: xtra::Handler<xtra_bitmex_price_feed::LatestQuote>,
{
    async fn limit_close(&self) -> Result<()> {
        let mut conn = self.db.acquire().await?;

        let latest_quote = self
            .price_feed_actor
            .send(xtra_bitmex_price_feed::LatestQuote)
            .await
            .context("Price feed not available")?
            .context("No quote available")?;

        let current_price = Price::new(latest_quote.for_taker())?;

        // TODO: Optimize for only loading all open cfds!
        let cfd_ids = load_all_cfd_ids(&mut conn).await?;

        for id in cfd_ids {
            let limit_close_price = load_limit_close_price(id, &mut conn).await?;

            match limit_close_price {
                None => {}
                Some(limit_close_price) => {
                    let (cfd, _) = load_cfd(id, &mut conn).await?;

                    match cfd.position {
                        Position::Long => {
                            if current_price >= limit_close_price {
                                self.taker_cfd_actor
                                    .send(taker_cfd::ProposeSettlement {
                                        order_id: id,
                                        current_price,
                                    })
                                    .await??;
                            }
                        }
                        Position::Short => {
                            todo!("short position auto-close not supported yet")
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl<O, W, P> xtra::Actor for Actor<O, W, P>
where
    O: xtra::Handler<taker_cfd::ProposeSettlement> + 'static,
    W: xtra::Handler<taker_cfd::ProposeSettlement> + 'static,
    P: xtra::Handler<xtra_bitmex_price_feed::LatestQuote> + 'static,
{
    type Stop = ();

    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        self.tasks
            .add(this.send_interval(Duration::from_secs(10), || Sync));
    }

    async fn stopped(self) -> Self::Stop {}
}

pub struct SetLimitClosePrice {
    pub id: OrderId,
    pub price: Option<Price>,
}

pub struct Sync;
