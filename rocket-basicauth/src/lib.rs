#![allow(clippy::let_unit_value)] // see: https://github.com/SergioBenitez/Rocket/issues/2211
use rocket::http::Header;
use rocket::http::Status;
use rocket::outcome::try_outcome;
use rocket::request::FromRequest;
use rocket::request::Outcome;
use rocket::Request;
use rocket::State;
use std::fmt;
use std::str;
use std::string::FromUtf8Error;
use void::Void;

/// A request guard that can be included in handler definitions to enforce authentication.
#[derive(Debug, Clone, Copy)]
pub struct Authenticated {}

#[derive(Debug)]
pub enum Error {
    UnknownUser(String),
    BadPassword,
    /// The contents of the header are not valid base64.
    NotBase64(base64::DecodeError),
    /// The base64-encoded bytes cannot be represented as a UTF8 string.
    NotUtf8(FromUtf8Error),
    /// Auth header did not follow the `username:password` format.
    InvalidBasicAuthFormat,
    /// The auth password was not configured in Rocket's state.
    MissingPassword,
    /// The auth username was not configured in Rocket's state.
    MissingUsername,
    NoAuthHeader,
    TooManyAuthHeaders,
}

#[derive(PartialEq, Eq, Debug, Clone, Copy)]
pub struct Username(pub &'static str);

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq<String> for Username {
    fn eq(&self, other: &String) -> bool {
        self.0.eq(other)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Password(String);

impl From<[u8; 32]> for Password {
    fn from(bytes: [u8; 32]) -> Self {
        Self(hex::encode(bytes))
    }
}

impl fmt::Display for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl PartialEq<String> for Password {
    fn eq(&self, other: &String) -> bool {
        self.0.eq(other)
    }
}

impl str::FromStr for Password {
    type Err = Void;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_owned()))
    }
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for Authenticated {
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

        let expected_username = try_outcome!(req
            .guard::<&'r State<Username>>()
            .await
            .map_failure(|(status, _)| (status, Error::MissingUsername)));
        let expected_password = try_outcome!(req
            .guard::<&'r State<Password>>()
            .await
            .map_failure(|(status, _)| (status, Error::MissingPassword)));

        if expected_username.inner() != &username {
            return Outcome::Failure((
                Status::Unauthorized,
                Error::UnknownUser(username.to_owned()),
            ));
        }

        if expected_password.inner() != &password {
            return Outcome::Failure((Status::Unauthorized, Error::BadPassword));
        }

        Outcome::Success(Authenticated {})
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

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::http::Status;
    use rocket::local::blocking::Client;
    use rocket::Build;
    use rocket::Rocket;

    #[test]
    fn routes_are_password_protected() {
        let client = Client::tracked(rocket()).unwrap();

        let response = client.get("/protected").dispatch();

        assert_eq!(response.status(), Status::Unauthorized);
        assert_eq!(
            response.headers().get_one("WWW-Authenticate"),
            Some(r#"Basic charset="UTF-8"#)
        );
    }

    #[test]
    fn correct_password_grants_access() {
        let client = Client::tracked(rocket()).unwrap();

        let response = client.get("/protected").header(auth_header()).dispatch();

        assert_eq!(response.status(), Status::Ok);
    }

    #[rocket::get("/protected")]
    async fn protected(_auth: Authenticated) {}

    /// Constructs a Rocket instance for testing.
    fn rocket() -> Rocket<Build> {
        rocket::build()
            .manage(Username("itchysats"))
            .manage(Password::from(*b"Now I'm feelin' so fly like a G6"))
            .mount("/", rocket::routes![protected])
            .register("/", rocket::catchers![unauthorized])
    }

    /// Creates an "Authorization" header that matches the password above,
    /// in particular it has been created through:
    /// ```
    /// base64(itchysats:hex("Now I'm feelin' so fly like a G6"))
    /// ```
    fn auth_header() -> Header<'static> {
        Header::new(
            "Authorization",
            "Basic aXRjaHlzYXRzOjRlNmY3NzIwNDkyNzZkMjA2NjY1NjU2YzY5NmUyNzIwNzM2ZjIwNjY2Yzc5MjA2YzY5NmI2NTIwNjEyMDQ3MzY=",
        )
    }
}
