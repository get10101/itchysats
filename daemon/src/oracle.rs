use crate::actors::log_error;
use anyhow::Result;
use async_trait::async_trait;
use cfd_protocol::secp256k1_zkp::{schnorrsig, SecretKey};
use futures::stream::FuturesOrdered;
use futures::TryStreamExt;
use rocket::time::{format_description, Duration, OffsetDateTime, Time};
use std::collections::HashSet;
use std::convert::TryFrom;

/// Where `olivia` is located.
const OLIVIA_URL: &str = "https://h00.ooo/";

const OLIVIA_EVENT_TIME_FORMAT: &str = "[year]-[month]-[day]T[hour]:[minute]:[second]";

pub struct Actor<CFD, M>
where
    CFD: xtra::Handler<Announcements> + xtra::Handler<Attestation>,
    M: xtra::Handler<Attestation>,
{
    latest_announcements: Option<[Announcement; 24]>,
    pending_attestations: HashSet<String>,
    cfd_actor_address: xtra::Address<CFD>,
    monitor_actor_address: xtra::Address<M>,
}

impl<CFD, M> Actor<CFD, M>
where
    CFD: xtra::Handler<Announcements> + xtra::Handler<Attestation>,
    M: xtra::Handler<Attestation>,
{
    pub fn new(
        cfd_actor_address: xtra::Address<CFD>,
        monitor_actor_address: xtra::Address<M>,
    ) -> Self {
        Self {
            latest_announcements: None,
            pending_attestations: HashSet::new(),
            cfd_actor_address,
            monitor_actor_address,
        }
    }

    async fn update_state(&mut self) -> Result<()> {
        self.update_latest_announcements().await?;
        self.update_pending_attestations().await?;

        Ok(())
    }

    async fn update_latest_announcements(&mut self) -> Result<()> {
        let new_announcements = next_urls()
            .into_iter()
            .map(|event_url| async {
                let announcement = reqwest::get(event_url)
                    .await?
                    .json::<Announcement>()
                    .await?;
                Result::<_, anyhow::Error>::Ok(announcement)
            })
            .collect::<FuturesOrdered<_>>()
            .try_collect::<Vec<_>>()
            .await?;

        let new_announcements = <[Announcement; 24]>::try_from(new_announcements)
            .map_err(|vec| anyhow::anyhow!("wrong number of announcements: {}", vec.len()))?;

        if self.latest_announcements.as_ref() != Some(&new_announcements) {
            self.latest_announcements = Some(new_announcements.clone());
            self.cfd_actor_address
                .do_send_async(Announcements(new_announcements))
                .await?;
        }

        Ok(())
    }

    async fn update_pending_attestations(&mut self) -> Result<()> {
        let pending_attestations = self.pending_attestations.clone();
        for event_id in pending_attestations.into_iter() {
            {
                let res = match reqwest::get(format!("{}{}", OLIVIA_URL, event_id)).await {
                    Ok(res) => res,
                    Err(e) => {
                        // TODO: Can we differentiate between errors?
                        tracing::warn!(%event_id, "Attestation not available: {}", e);
                        continue;
                    }
                };

                let attestation = res.json::<Attestation>().await?;

                self.cfd_actor_address
                    .clone()
                    .do_send_async(attestation.clone())
                    .await?;
                self.monitor_actor_address
                    .clone()
                    .do_send_async(attestation)
                    .await?;

                self.pending_attestations.remove(&event_id);
            }
        }

        Ok(())
    }

    fn monitor_event(&mut self, event_id: String) {
        if !self.pending_attestations.insert(event_id.clone()) {
            tracing::trace!("Event {} already being monitored", event_id);
        }
    }
}

impl<CFD, M> xtra::Actor for Actor<CFD, M>
where
    CFD: xtra::Handler<Announcements> + xtra::Handler<Attestation>,
    M: xtra::Handler<Attestation>,
{
}

pub struct Sync;

impl xtra::Message for Sync {
    type Result = ();
}

#[async_trait]
impl<CFD, M> xtra::Handler<Sync> for Actor<CFD, M>
where
    CFD: xtra::Handler<Announcements> + xtra::Handler<Attestation>,
    M: xtra::Handler<Attestation>,
{
    async fn handle(&mut self, _: Sync, _ctx: &mut xtra::Context<Self>) {
        log_error!(self.update_state())
    }
}

pub struct MonitorEvent {
    pub event_id: String,
}

impl xtra::Message for MonitorEvent {
    type Result = ();
}

#[async_trait]
impl<CFD, M> xtra::Handler<MonitorEvent> for Actor<CFD, M>
where
    CFD: xtra::Handler<Announcements> + xtra::Handler<Attestation>,
    M: xtra::Handler<Attestation>,
{
    async fn handle(&mut self, msg: MonitorEvent, _ctx: &mut xtra::Context<Self>) {
        self.monitor_event(msg.event_id)
    }
}

/// Construct the URL of the next 24 `BitMEX/BXBT` hourly events
/// `olivia` will attest to.
fn next_urls() -> Vec<String> {
    next_24_hours(OffsetDateTime::now_utc())
        .into_iter()
        .map(event_url)
        .collect()
}

fn next_24_hours(datetime: OffsetDateTime) -> Vec<OffsetDateTime> {
    let adjusted = datetime.replace_time(Time::from_hms(datetime.hour(), 0, 0).expect("in_range"));
    (1..=24).map(|i| adjusted + Duration::hours(i)).collect()
}

/// Construct the URL of `olivia`'s `BitMEX/BXBT` event to be attested
/// for at the time indicated by the argument `datetime`.
fn event_url(datetime: OffsetDateTime) -> String {
    let formatter = format_description::parse(OLIVIA_EVENT_TIME_FORMAT).expect("valid formatter");
    datetime
        .format(&formatter)
        .expect("valid formatter for datetime");

    format!("{}/x/BitMEX/BXBT/{}.price[n:20]", OLIVIA_URL, datetime)
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq)]
#[serde(from = "olivia_api::Announcement")]
pub struct Announcement {
    /// Identifier for an oracle event.
    ///
    /// Doubles up as the path of the URL for this event i.e.
    /// https://h00.ooo/{id}.
    pub id: String,
    pub expected_outcome_time: OffsetDateTime,
    pub nonce_pks: Vec<schnorrsig::PublicKey>,
}

pub struct Announcements(pub [Announcement; 24]);

// TODO: Implement real deserialization once price attestation is
// implemented in `olivia`
#[derive(Debug, Clone, serde::Deserialize)]
pub struct Attestation {
    pub id: String,
    pub price: u64,
    pub scalars: Vec<SecretKey>,
}

impl xtra::Message for Announcements {
    type Result = ();
}

impl xtra::Message for Attestation {
    type Result = ();
}

mod olivia_api {
    use cfd_protocol::secp256k1_zkp::schnorrsig;
    use time::OffsetDateTime;

    impl From<Announcement> for super::Announcement {
        fn from(announcement: Announcement) -> Self {
            Self {
                id: announcement.id,
                expected_outcome_time: announcement.expected_outcome_time,
                nonce_pks: announcement.schemes.olivia_v1.nonces,
            }
        }
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    pub(crate) struct Announcement {
        id: String,
        #[serde(rename = "expected-outcome-time")]
        #[serde(with = "timestamp")]
        expected_outcome_time: OffsetDateTime,
        schemes: Schemes,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct Schemes {
        #[serde(rename = "olivia-v1")]
        olivia_v1: OliviaV1,
    }

    #[derive(Debug, Clone, serde::Deserialize)]
    struct OliviaV1 {
        nonces: Vec<schnorrsig::PublicKey>,
    }

    mod timestamp {
        use crate::oracle::OLIVIA_EVENT_TIME_FORMAT;
        use serde::de::Error as _;
        use serde::{Deserialize, Deserializer};
        use time::{format_description, OffsetDateTime, PrimitiveDateTime};

        pub fn deserialize<'a, D>(deserializer: D) -> Result<OffsetDateTime, D::Error>
        where
            D: Deserializer<'a>,
        {
            let string = String::deserialize(deserializer)?;
            let format = format_description::parse(OLIVIA_EVENT_TIME_FORMAT).expect("valid format");

            let date_time = PrimitiveDateTime::parse(&string, &format).map_err(D::Error::custom)?;

            Ok(date_time.assume_utc())
        }
    }

    #[cfg(test)]
    mod tests {
        use std::str::FromStr;

        use crate::oracle;
        use cfd_protocol::secp256k1_zkp::schnorrsig;
        use time::macros::datetime;

        #[test]
        fn deserialize_announcement() {
            let json = r#"
 {
        "id": "/BitMEX/BXBT/2021-09-28T03:20:00/price/",
        "expected-outcome-time": "2021-09-28T03:20:00",
        "descriptor": {
                "type": "enum",
                "outcomes": [
                        "0",
                        "1"
                ]
        },
        "schemes": {
                "olivia-v1": {
                        "nonces": [
                                "41a26c4853f2edc5604069541fdd1df103b52c31e959451d115f8220aeb8b414"
                        ]
                },
                "ecdsa-v1": {}
        }
}
"#;

            let deserialized = serde_json::from_str::<oracle::Announcement>(json).unwrap();
            let expected = oracle::Announcement {
                id: "/BitMEX/BXBT/2021-09-28T03:20:00/price/".to_string(),
                expected_outcome_time: datetime!(2021-09-28 03:20:00).assume_utc(),
                nonce_pks: vec![schnorrsig::PublicKey::from_str(
                    "41a26c4853f2edc5604069541fdd1df103b52c31e959451d115f8220aeb8b414",
                )
                .unwrap()],
            };

            assert_eq!(deserialized, expected)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::format_description;
    use time::macros::datetime;

    #[test]
    fn next_event_url_is_correct() {
        let datetime = datetime!(2021-09-23 10:43:00);
        let format = format_description::parse(OLIVIA_EVENT_TIME_FORMAT).unwrap();

        let date_time_formatted = datetime.format(&format).unwrap();
        let expected = "2021-09-23T10:43:00";

        assert_eq!(date_time_formatted, expected);
    }

    #[test]
    fn next_24() {
        let datetime = datetime!(2021-09-23 10:43:12);

        let next_24_hours = next_24_hours(datetime.assume_utc());
        let expected = vec![
            datetime!(2021-09-23 11:00:00).assume_utc(),
            datetime!(2021-09-23 12:00:00).assume_utc(),
            datetime!(2021-09-23 13:00:00).assume_utc(),
            datetime!(2021-09-23 14:00:00).assume_utc(),
            datetime!(2021-09-23 15:00:00).assume_utc(),
            datetime!(2021-09-23 16:00:00).assume_utc(),
            datetime!(2021-09-23 17:00:00).assume_utc(),
            datetime!(2021-09-23 18:00:00).assume_utc(),
            datetime!(2021-09-23 19:00:00).assume_utc(),
            datetime!(2021-09-23 20:00:00).assume_utc(),
            datetime!(2021-09-23 21:00:00).assume_utc(),
            datetime!(2021-09-23 22:00:00).assume_utc(),
            datetime!(2021-09-23 23:00:00).assume_utc(),
            datetime!(2021-09-24 00:00:00).assume_utc(),
            datetime!(2021-09-24 01:00:00).assume_utc(),
            datetime!(2021-09-24 02:00:00).assume_utc(),
            datetime!(2021-09-24 03:00:00).assume_utc(),
            datetime!(2021-09-24 04:00:00).assume_utc(),
            datetime!(2021-09-24 05:00:00).assume_utc(),
            datetime!(2021-09-24 06:00:00).assume_utc(),
            datetime!(2021-09-24 07:00:00).assume_utc(),
            datetime!(2021-09-24 08:00:00).assume_utc(),
            datetime!(2021-09-24 09:00:00).assume_utc(),
            datetime!(2021-09-24 10:00:00).assume_utc(),
        ];

        assert_eq!(next_24_hours, expected)
    }
}
