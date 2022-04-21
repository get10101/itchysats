use crate::command;
use crate::db;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use maia::secp256k1_zkp::schnorrsig;
use model::olivia;
use model::olivia::BitMexPriceEventId;
use model::CfdEvent;
use model::EventKind;
use std::collections::HashMap;
use std::collections::HashSet;
use std::ops::Add;
use time::ext::NumericalDuration;
use time::Duration;
use time::OffsetDateTime;
use time::Time;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

pub struct Actor {
    announcements: HashMap<BitMexPriceEventId, (OffsetDateTime, Vec<schnorrsig::PublicKey>)>,
    pending_attestations: HashSet<BitMexPriceEventId>,
    executor: command::Executor,
    announcement_lookahead: Duration,
    tasks: Tasks,
    db: db::Connection,
    client: reqwest::Client,
}

#[derive(Clone, Copy)]
pub struct Sync;

#[derive(Clone, Copy)]
pub struct MonitorAttestation {
    pub event_id: BitMexPriceEventId,
}

/// Message used to request the `Announcement` from the
/// `oracle::Actor`'s local state.
///
/// The `Announcement` corresponds to the [`BitMexPriceEventId`] included in
/// the message.
#[derive(Clone, Copy)]
pub struct GetAnnouncement(pub BitMexPriceEventId);

#[derive(Debug, Clone)]
pub struct Attestation(olivia::Attestation);

/// A module-private message to allow parallelization of fetching announcements.
#[derive(Debug)]
struct NewAnnouncementFetched {
    id: BitMexPriceEventId,
    expected_outcome_time: OffsetDateTime,
    nonce_pks: Vec<schnorrsig::PublicKey>,
}

/// A module-private message to allow parallelization of fetching attestations.
#[derive(Debug)]
struct NewAttestationFetched {
    id: BitMexPriceEventId,
    attestation: Attestation,
}

#[derive(Default, Clone)]
struct Cfd {
    pending_attestation: Option<BitMexPriceEventId>,
    version: u32,
}

impl Cfd {
    fn apply(mut self, event: CfdEvent) -> Self {
        self.version += 1;

        let settlement_event_id = match event.event {
            EventKind::ContractSetupCompleted { dlc, .. } => dlc.settlement_event_id,
            EventKind::RolloverCompleted { dlc, .. } => dlc.settlement_event_id,
            // TODO: There might be a few cases where we do not need to monitor the attestation,
            // e.g. when we already agreed to collab. settle. Ignoring it for now
            // because I don't want to think about it and it doesn't cause much harm to do the
            // monitoring :)
            _ => return self,
        };

        // we can comfortably overwrite what was there because events are processed in order, thus
        // old attestations don't matter.
        Self {
            pending_attestation: Some(settlement_event_id),
            ..self
        }
    }
}

impl db::CfdAggregate for Cfd {
    type CtorArgs = ();

    fn new(_: Self::CtorArgs, _: db::Cfd) -> Self {
        Self::default()
    }

    fn apply(self, event: CfdEvent) -> Self {
        self.apply(event)
    }

    fn version(&self) -> u32 {
        self.version
    }
}

impl Actor {
    pub fn new(
        db: db::Connection,
        executor: command::Executor,
        announcement_lookahead: Duration,
    ) -> Self {
        Self {
            announcements: HashMap::new(),
            pending_attestations: HashSet::new(),
            executor,
            announcement_lookahead,
            tasks: Tasks::default(),
            db,
            client: reqwest::Client::new(),
        }
    }

    fn ensure_having_announcements(
        &mut self,
        announcement_lookahead: Duration,
        ctx: &mut xtra::Context<Self>,
    ) {
        // we want inclusive the settlement_time_interval_hours length hence +1
        for hour in 1..announcement_lookahead.whole_hours() + 1 {
            let event_id =
                next_announcement_after(time::OffsetDateTime::now_utc() + Duration::hours(hour));

            if self.announcements.get(&event_id).is_some() {
                continue;
            }
            let this = ctx.address().expect("self to be alive");
            let client = self.client.clone();

            self.tasks.add_fallible(
                async move {
                    let url = event_id.to_olivia_url();

                    tracing::debug!("Fetching announcement for {event_id}");

                    let response = client
                        .get(url.clone())
                        .send()
                        .await
                        .with_context(|| format!("Failed to GET {url}"))?;

                    let code = response.status();
                    if !code.is_success() {
                        anyhow::bail!("GET {url} responded with {code}");
                    }

                    let announcement = response
                        .json::<olivia::Announcement>()
                        .await
                        .context("Failed to deserialize as Announcement")?;

                    this.send(NewAnnouncementFetched {
                        id: event_id,
                        nonce_pks: announcement.nonce_pks,
                        expected_outcome_time: announcement.expected_outcome_time,
                    })
                    .await?;

                    Ok(())
                },
                |e| async move {
                    tracing::debug!("Failed to fetch announcement: {:#}", e);
                },
            );
        }
    }

    fn update_pending_attestations(&mut self, ctx: &mut xtra::Context<Self>) {
        for event_id in self.pending_attestations.iter().copied() {
            if !event_id.has_likely_occured() {
                tracing::trace!("Skipping {event_id} because it likely hasn't occurred yet");

                continue;
            }

            let this = ctx.address().expect("self to be alive");
            let client = self.client.clone();

            self.tasks.add_fallible(
                async move {
                    let url = event_id.to_olivia_url();

                    tracing::debug!("Fetching attestation for {event_id}");

                    let response = client
                        .get(url.clone())
                        .send()
                        .await
                        .with_context(|| format!("Failed to GET {url}"))?;

                    let code = response.status();
                    if !code.is_success() {
                        anyhow::bail!("GET {url} responded with {code}");
                    }

                    let attestation = response
                        .json::<olivia::Attestation>()
                        .await
                        .context("Failed to deserialize as Attestation")?;

                    this.send(NewAttestationFetched {
                        id: event_id,
                        attestation: Attestation(attestation),
                    })
                    .await??;

                    Ok(())
                },
                |e| async move {
                    tracing::debug!("Failed to fetch attestation: {:#}", e);
                },
            )
        }
    }
}

#[xtra_productivity]
impl Actor {
    fn handle_monitor_attestation(&mut self, msg: MonitorAttestation) {
        let price_event_id = msg.event_id;

        if !self.pending_attestations.insert(price_event_id) {
            tracing::trace!("Attestation {price_event_id} already being monitored");
        }
    }

    fn handle_get_announcement(
        &mut self,
        msg: GetAnnouncement,
    ) -> Result<olivia::Announcement, NoAnnouncement> {
        self.announcements
            .get_key_value(&msg.0)
            .map(|(id, (time, nonce_pks))| olivia::Announcement {
                id: *id,
                expected_outcome_time: *time,
                nonce_pks: nonce_pks.clone(),
            })
            .ok_or(NoAnnouncement(msg.0))
    }

    fn handle_new_announcement_fetched(&mut self, msg: NewAnnouncementFetched) {
        self.announcements
            .insert(msg.id, (msg.expected_outcome_time, msg.nonce_pks));
    }

    fn handle_sync(&mut self, _: Sync, ctx: &mut xtra::Context<Self>) {
        self.ensure_having_announcements(self.announcement_lookahead, ctx);
        self.update_pending_attestations(ctx);
    }

    async fn handle_new_attestation_fetched(&mut self, msg: NewAttestationFetched) -> Result<()> {
        let NewAttestationFetched { id, attestation } = msg;

        tracing::info!("Fetched new attestation for {id}");

        for id in self.db.load_open_cfd_ids().await? {
            if let Err(err) = self
                .executor
                .execute(id, |cfd| cfd.decrypt_cet(&attestation.0))
                .await
            {
                tracing::warn!(order_id = %id, "Failed to decrypt CET using attestation: {}", err)
            }
        }

        self.pending_attestations.remove(&id);

        Ok(())
    }
}

#[derive(Debug, Clone, thiserror::Error, Copy)]
#[error("Announcement {0} not found")]
pub struct NoAnnouncement(pub BitMexPriceEventId);

pub fn next_announcement_after(timestamp: OffsetDateTime) -> BitMexPriceEventId {
    let adjusted = ceil_to_next_hour(timestamp);

    BitMexPriceEventId::with_20_digits(adjusted)
}

fn ceil_to_next_hour(original: OffsetDateTime) -> OffsetDateTime {
    let timestamp = original.add(1.hours());
    let exact_hour = Time::from_hms(timestamp.hour(), 0, 0).expect(
        "Exact hour from timestamp to be always within range, both docs and tests confirm it",
    );
    timestamp.replace_time(exact_hour)
}

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        self.tasks.add(
            this.clone()
                .send_interval(std::time::Duration::from_secs(5), || Sync),
        );

        self.tasks.add({
            let db = self.db.clone();

            async move {
                let mut stream = db.load_all_open_cfds::<Cfd>(());

                while let Some(cfd) = stream.next().await {
                    let Cfd {
                        pending_attestation,
                        ..
                    } = match cfd {
                        Ok(cfd) => cfd,
                        Err(e) => {
                            tracing::warn!("Failed to load CFD from database: {e:#}");
                            continue;
                        }
                    };
                    if let Some(pending_attestation) = pending_attestation {
                        let _: Result<(), xtra::Disconnected> = this
                            .send(MonitorAttestation {
                                event_id: pending_attestation,
                            })
                            .await;
                    }
                }
            }
        });
    }

    async fn stopped(self) -> Self::Stop {}
}

impl Attestation {
    pub fn new(attestation: olivia::Attestation) -> Self {
        Self(attestation)
    }

    pub fn as_inner(&self) -> &olivia::Attestation {
        &self.0
    }

    pub fn into_inner(self) -> olivia::Attestation {
        self.0
    }

    pub fn id(&self) -> BitMexPriceEventId {
        self.0.id
    }
}

impl xtra::Message for Attestation {
    type Result = ();
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::macros::datetime;

    #[test]
    fn next_event_id_after_timestamp() {
        let event_id = next_announcement_after(datetime!(2021-09-23 10:40:00).assume_utc());

        assert_eq!(
            event_id.to_string(),
            "/x/BitMEX/BXBT/2021-09-23T11:00:00.price?n=20"
        );
    }

    #[test]
    fn next_event_id_is_midnight_next_day() {
        let event_id = next_announcement_after(datetime!(2021-09-23 23:40:00).assume_utc());

        assert_eq!(
            event_id.to_string(),
            "/x/BitMEX/BXBT/2021-09-24T00:00:00.price?n=20"
        );
    }
}
