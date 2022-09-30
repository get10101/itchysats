use crate::error::Error;
use crate::forms::Login;
use crate::session::Session;
use crate::session::AUTH_COOKIE;
use crate::user::User;
use anyhow::Context;
use anyhow::Result;
use rocket::http::Cookie;
use rocket::http::CookieJar;
use rocket::http::Status;
use rocket::request::FromRequest;
use rocket::request::Outcome;
use rocket::Request;
use rocket::State;
use serde_json::json;

use crate::users::Users;
use rand::random;

pub fn rand_string(size: usize) -> String {
    (0..)
        .map(|_| random::<char>())
        .filter(|c| c.is_ascii())
        .map(char::from)
        .take(size)
        .collect()
}

/// The [`Auth`] guard allows to log in, log out, sign up, modify, the currently
/// (un)authenticated user. For more information see [`Auth`].
pub struct Auth<'a> {
    /// `Auth` includes in its fields a [`Users`] instance. Therefore, it is not necessary to
    /// retrieve `Users` when using this guard.
    pub users: &'a State<Users>,
    pub cookies: &'a CookieJar<'a>,
    pub session: Option<Session>,
}

#[async_trait]
impl<'r> FromRequest<'r> for Auth<'r> {
    type Error = Error;
    async fn from_request(req: &'r Request<'_>) -> Outcome<Auth<'r>, Error> {
        let session: Option<Session> = if let Outcome::Success(users) = req.guard().await {
            Some(users)
        } else {
            None
        };
        tracing::info!("Session guard in auth: {session:?}");

        let users: &State<Users> = if let Outcome::Success(users) = req.guard().await {
            users
        } else {
            return Outcome::Failure((Status::InternalServerError, Error::UnmanagedState));
        };

        Outcome::Success(Auth {
            users,
            session,
            cookies: req.cookies(),
        })
    }
}

impl<'a> Auth<'a> {
    /// Logs in the user with a session set to expire in one week.
    pub async fn login(&self, form: &Login) -> Result<User, Error> {
        let user = self.users.login(form).await?;
        let session = Session {
            id: user.id,
            auth_key: user.auth_key.clone(),
        };
        let to_str = format!("{}", json!(session));
        self.cookies.add_private(Cookie::new(AUTH_COOKIE, to_str));
        Ok(user)
    }

    /// Verifies if the provided session is authenticated
    pub fn is_auth(&self) -> Result<bool> {
        if let Some(session) = &self.session {
            tracing::info!("Session : {session:?}");
            Ok(self.users.is_auth(session)?)
        } else {
            Ok(false)
        }
    }

    /// Retrieve the current logged in user from the session or return None if not logged in.
    pub async fn get_user(&self) -> Result<Option<User>> {
        if !self.is_auth()? {
            return Ok(None);
        }
        let id = self.session.as_ref().context("Could not get session")?.id;
        if let Ok(Some(user)) = self.users.get_by_id().await {
            Ok(Some(User {
                id,
                password: user.password,
                auth_key: "NONE".to_string(),
                first_login: user.first_login,
            }))
        } else {
            Ok(None)
        }
    }

    pub fn logout(&self) -> Result<()> {
        match self.get_session() {
            Ok(session) => {
                if let Err(e) = self.users.logout(session) {
                    tracing::warn!("Could not get session {e:#}")
                }
            }
            Err(e) => {
                tracing::warn!("Could not get session: {e:#}")
            }
        }
        // we need to remove the cookie in any case otherwise a failed logout may not be recoverable
        self.cookies.remove_private(Cookie::named(AUTH_COOKIE));
        Ok(())
    }

    pub fn get_session(&self) -> Result<&Session> {
        let session = self.session.as_ref().ok_or(Error::Unauthenticated)?;
        Ok(session)
    }
}
