use crate::command;
use anyhow::Context;
use anyhow::Result;
use async_trait::async_trait;
use futures::StreamExt;
use maia_core::secp256k1_zkp::schnorrsig;
use model::olivia;
use model::olivia::next_announcement_after;
use model::olivia::BitMexPriceEventId;
use model::CfdEvent;
use model::EventKind;
use sqlite_db;
use std::collections::HashMap;
use std::collections::HashSet;
use time::Duration;
use time::OffsetDateTime;
use tokio_tasks::Tasks;
use xtra_productivity::xtra_productivity;
use xtras::SendInterval;

/// Timout to be passed into the reqwest client for doing http requests against the oracle.
///
/// 10 seconds was chosen arbitrarily. It should be plenty to fetch from the oracle and does not let
/// us wait forever.
const REQWEST_TIMEOUT: core::time::Duration = core::time::Duration::from_secs(10);

/// We only have to sync for new announcements once an hour.
///
/// Syncing every 60 seconds might still be an overkill but should not hurt us.
const SYNC_ANNOUNCEMENTS_INTERVAL: core::time::Duration = std::time::Duration::from_secs(60);

/// We want to sync attestations fast but don't spam our internal actor. Hence, we chose 30 seconds.
const SYNC_ATTESTATIONS_INTERVAL: core::time::Duration = std::time::Duration::from_secs(30);

pub struct Actor {
    announcements: HashMap<BitMexPriceEventId, (OffsetDateTime, Vec<schnorrsig::PublicKey>)>,
    pending_attestations: HashSet<BitMexPriceEventId>,
    executor: command::Executor,
    announcement_lookahead: Duration,
    tasks: Tasks,
    db: sqlite_db::Connection,
    client: reqwest::Client,
}

#[derive(Clone, Copy)]
pub struct SyncAnnouncements;

#[derive(Clone, Copy)]
pub struct SyncAttestations;

#[derive(Clone, Copy)]
pub struct MonitorAttestation {
    pub event_id: BitMexPriceEventId,
}

#[derive(Clone)]
struct MonitorAttestations {
    pub event_ids: Vec<BitMexPriceEventId>,
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
            EventKind::ContractSetupCompleted { dlc: None, .. } => return self,
            EventKind::ContractSetupCompleted { dlc: Some(dlc), .. } => dlc.settlement_event_id,
            EventKind::RolloverCompleted { dlc: None, .. } => return self,
            EventKind::RolloverCompleted { dlc: Some(dlc), .. } => dlc.settlement_event_id,
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

impl sqlite_db::CfdAggregate for Cfd {
    type CtorArgs = ();

    fn new(_: Self::CtorArgs, _: sqlite_db::Cfd) -> Self {
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
        db: sqlite_db::Connection,
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

                    tracing::debug!(event_id = %event_id, "Fetching announcement");

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
            if !event_id.has_likely_occurred() {
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
                        .timeout(REQWEST_TIMEOUT)
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

    fn add_pending_attestation(&mut self, event_id: BitMexPriceEventId) {
        if !self.pending_attestations.insert(event_id) {
            tracing::trace!("Attestation for {event_id} already being monitored");
        }
    }
}

#[xtra_productivity]
impl Actor {
    fn handle_monitor_attestation(&mut self, msg: MonitorAttestation) {
        self.add_pending_attestation(msg.event_id)
    }

    fn handle_monitor_attestations(&mut self, msg: MonitorAttestations) {
        for id in msg.event_ids.into_iter() {
            self.add_pending_attestation(id);
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

    fn handle_sync_announcements(&mut self, _: SyncAnnouncements, ctx: &mut xtra::Context<Self>) {
        self.ensure_having_announcements(self.announcement_lookahead, ctx);
    }

    fn handle_sync_attestations(&mut self, _: SyncAttestations, ctx: &mut xtra::Context<Self>) {
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
                tracing::warn!(order_id = %id, "Failed to decrypt CET using attestation: {err:#}")
            }
        }

        self.pending_attestations.remove(&id);

        Ok(())
    }
}

#[derive(Debug, Clone, thiserror::Error, Copy)]
#[error("Announcement {0} not found")]
pub struct NoAnnouncement(pub BitMexPriceEventId);

#[async_trait]
impl xtra::Actor for Actor {
    type Stop = ();
    async fn started(&mut self, ctx: &mut xtra::Context<Self>) {
        let this = ctx.address().expect("we are alive");
        self.tasks.add(
            this.clone()
                .send_interval(SYNC_ANNOUNCEMENTS_INTERVAL, || SyncAnnouncements),
        );

        self.tasks.add({
            let db = self.db.clone();
            async move {
                let pending_attestations = db
                    .load_all_open_cfds::<Cfd>(())
                    .filter_map(|res| async move {
                        match res {
                            Ok(Cfd {
                                pending_attestation,
                                ..
                            }) => pending_attestation,
                            Err(e) => {
                                tracing::warn!("Failed to load CFD from database: {e:#}");
                                None
                            }
                        }
                    })
                    .collect::<Vec<_>>()
                    .await;

                let _: Result<(), xtra::Error> = this
                    .send(MonitorAttestations {
                        event_ids: pending_attestations,
                    })
                    .await;

                this.send_interval(SYNC_ATTESTATIONS_INTERVAL, || SyncAttestations)
                    .await;
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
