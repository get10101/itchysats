use crate::error::Error;
use anyhow::Context;
use anyhow::Result;
use chashmap::CHashMap;
use rocket::http::CookieJar;
use rocket::http::Status;
use rocket::request::FromRequest;
use rocket::request::Outcome;
use rocket::request::Request;
use serde::Deserialize;
use serde::Serialize;
use serde_json::from_str;
use time::Duration;

pub const AUTH_COOKIE: &str = "itchysats_auth";

/// Represents a user session
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Session {
    /// The user id as it is stored on the database.
    pub id: u32,
    /// A random authentication token key.
    pub auth_key: String,
}

#[async_trait]
impl<'r> FromRequest<'r> for Session {
    type Error = Error;
    async fn from_request(request: &'r Request<'_>) -> Outcome<Session, Self::Error> {
        let cookies = request.cookies();

        match get_session(cookies) {
            Ok(session) => Outcome::Success(session),
            Err(_e) => Outcome::Failure((Status::Unauthorized, Error::Unauthorized)),
        }
    }
}

fn get_session(cookies: &CookieJar) -> Result<Session> {
    let session = cookies
        .get_private(AUTH_COOKIE)
        .context("Cookie not found")?;
    tracing::info!("Get session: {session}");
    let result = from_str(session.value()).map_err(Error::Serde)?;
    Ok(result)
}

pub trait SessionManager: Send + Sync {
    fn insert_for(&self, id: u32, key: String, time: Duration);
    fn remove(&self, id: u32);
    fn get(&self, id: u32) -> Result<Option<String>>;
}

impl SessionManager for CHashMap<u32, AuthKey> {
    fn insert_for(&self, id: u32, key: String, time: Duration) {
        let key = AuthKey {
            expires: time.whole_seconds(),
            secret: key,
        };
        self.insert(id, key);
    }

    fn remove(&self, id: u32) {
        self.remove(&id);
    }

    fn get(&self, id: u32) -> Result<Option<String>> {
        let key = self
            .get(&id)
            .context("Could not acquire lock on session store")?;
        Ok(Some(key.secret.clone()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthKey {
    /// When the session is set to expire
    expires: i64,
    /// The session secret
    secret: String,
}

impl From<String> for AuthKey {
    fn from(secret: String) -> AuthKey {
        AuthKey {
            /// Default expiry is 7 days in seconds
            expires: 604800,
            secret,
        }
    }
}

impl From<&str> for AuthKey {
    fn from(secret: &str) -> AuthKey {
        AuthKey {
            /// Default expiry is 7 days in seconds
            expires: 604800,
            secret: secret.into(),
        }
    }
}
