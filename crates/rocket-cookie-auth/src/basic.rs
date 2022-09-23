#![allow(clippy::let_unit_value)] // see: https://github.com/SergioBenitez/Rocket/issues/2211
use crate::forms::Login;
use crate::users::Users;
use rocket::http::Header;
use rocket::http::Status;
use rocket::request::FromRequest;
use rocket::request::Outcome;
use rocket::Request;
use rocket::State;
use std::str;
use std::string::FromUtf8Error;

/// A request guard that can be included in handler definitions to enforce authentication.
#[derive(Debug, Clone, Copy)]
pub struct BasicAuthGuard {
    _priv: (),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Unknown username")]
    UnknownUser(String),
    #[error("Incorrect password")]
    BadPassword,
    #[error("Authorization header not valid base64")]
    NotBase64(base64::DecodeError),
    #[error("base64 not valid utf8")]
    NotUtf8(FromUtf8Error),
    #[error("Authorization header not in `username:password` format")]
    InvalidBasicAuthFormat,
    /// This error is thrown when trying to retrieve `Users` but it isn't being managed by the app.
    /// It can be fixed adding `.manage(users)` to the app, where `users` is of type `Users`.
    #[error("UnmanagedStateError: failed retrieving `Users`. You may be missing `.manage(users)` in your app.")]
    UnmanagedState,
    #[error("Authorization header missing")]
    NoAuthHeader,
    #[error("Only one value for Authorization error is permitted")]
    TooManyAuthHeaders,
}

const EXPECTED_USERNAME: &str = "itchysats";

#[rocket::async_trait]
impl<'r> FromRequest<'r> for BasicAuthGuard {
    type Error = Error;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let auth_headers = req.headers().get("Authorization").collect::<Vec<_>>();

        let (username, password) = match auth_headers.as_slice() {
            [] => return Outcome::Failure((Status::Unauthorized, Error::NoAuthHeader)),
            [header] => match decode_header(header) {
                Ok((username, password)) => (username, password),
                Err(e) => return Outcome::Failure((Status::Unauthorized, e)),
            },
            _too_many => return Outcome::Failure((Status::BadRequest, Error::TooManyAuthHeaders)),
        };

        let users: &State<Users> = if let Outcome::Success(users) = req.guard().await {
            users
        } else {
            return Outcome::Failure((Status::InternalServerError, Error::UnmanagedState));
        };

        if username != EXPECTED_USERNAME {
            return Outcome::Failure((
                Status::Unauthorized,
                Error::UnknownUser(username.to_owned()),
            ));
        }

        if users.login_basicauth(&Login { password }).await.is_err() {
            Outcome::Failure((Status::Unauthorized, Error::BadPassword))
        } else {
            Outcome::Success(BasicAuthGuard { _priv: () })
        }
    }
}

fn decode_header(header_value: &str) -> Result<(String, String), Error> {
    let base64 = header_value.trim_start_matches("Basic ");

    let decoded = base64::decode(base64).map_err(Error::NotBase64)?;
    let decoded = String::from_utf8(decoded).map_err(Error::NotUtf8)?;
    let (username, password) = decoded
        .split_once(':')
        .ok_or(Error::InvalidBasicAuthFormat)?;

    Ok((username.to_owned(), password.to_owned()))
}

/// A "catcher" for all 401 responses, triggers the browser's basic auth implementation.
#[rocket::catch(401)]
pub fn unauthorized() -> PromptAuthentication {
    PromptAuthentication {
        inner: (),
        www_authenticate: Header::new("WWW-Authenticate", r#"Basic charset="UTF-8"#),
    }
}

/// A rocket responder that prompts the user to sign in to access the API.
#[derive(Debug, rocket::Responder)]
#[response(status = 401)]
pub struct PromptAuthentication {
    inner: (),
    www_authenticate: Header<'static>,
}
