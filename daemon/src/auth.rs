use hex::FromHexError;
use rocket::http::Status;
use rocket::outcome::try_outcome;
use rocket::outcome::IntoOutcome;
use rocket::request::FromRequest;
use rocket::request::Outcome;
use rocket::Request;
use rocket::State;
use rocket_basicauth::BasicAuth;
use rocket_basicauth::BasicAuthError;
use std::fmt;
use std::str::FromStr;

/// A request guard that can be included in handler definitions to enforce authentication.
pub struct Authenticated {}

pub const USERNAME: &str = "itchysats";

#[derive(Debug)]
pub enum Error {
    UnknownUser(String),
    BadPassword,
    InvalidEncoding(FromHexError),
    BadBasicAuthHeader(BasicAuthError),
    /// The auth password was not configured in Rocket's state.
    MissingPassword,
    NoAuthHeader,
}

#[derive(PartialEq)]
pub struct Password([u8; 32]);

impl From<[u8; 32]> for Password {
    fn from(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl fmt::Display for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

impl FromStr for Password {
    type Err = FromHexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(s, &mut bytes)?;

        Ok(Self(bytes))
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Authenticated {
    type Error = Error;

    async fn from_request(req: &'r Request<'_>) -> Outcome<Self, Self::Error> {
        let basic_auth = try_outcome!(req
            .guard::<BasicAuth>()
            .await
            .map_failure(|(status, error)| (status, Error::BadBasicAuthHeader(error)))
            .forward_then(|()| Outcome::Failure((Status::Unauthorized, Error::NoAuthHeader))));
        let password = try_outcome!(req
            .guard::<&'r State<Password>>()
            .await
            .map_failure(|(status, _)| (status, Error::MissingPassword)));

        if basic_auth.username != USERNAME {
            return Outcome::Failure((
                Status::Unauthorized,
                Error::UnknownUser(basic_auth.username),
            ));
        }

        if &try_outcome!(basic_auth
            .password
            .parse::<Password>()
            .map_err(Error::InvalidEncoding)
            .into_outcome(Status::BadRequest))
            != password.inner()
        {
            return Outcome::Failure((Status::Unauthorized, Error::BadPassword));
        }

        Outcome::Success(Authenticated {})
    }
}
