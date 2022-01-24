use hex::FromHexError;
use rocket::http::Header;
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

#[derive(Debug)]
pub enum Error {
    UnknownUser(String),
    BadPassword,
    InvalidEncoding(FromHexError),
    BadBasicAuthHeader(BasicAuthError),
    /// The auth password was not configured in Rocket's state.
    MissingPassword,
    /// The auth username was not configured in Rocket's state.
    MissingUsername,
    NoAuthHeader,
}

#[derive(PartialEq)]
pub struct Username(pub &'static str);

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(PartialEq)]
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

impl FromStr for Password {
    type Err = FromHexError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(s.to_string()))
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
        let username = try_outcome!(req
            .guard::<&'r State<Username>>()
            .await
            .map_failure(|(status, _)| (status, Error::MissingUsername)));
        let password = try_outcome!(req
            .guard::<&'r State<Password>>()
            .await
            .map_failure(|(status, _)| (status, Error::MissingPassword)));

        if basic_auth.username != username.0 {
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

/// A "catcher" for all 401 responses, triggers the browser's basic auth implementation.
#[rocket::catch(401)]
pub fn unauthorized() -> PromptAuthentication {
    PromptAuthentication {
        inner: (),
        www_authenticate: Header::new("WWW-Authenticate", r#"Basic charset="UTF-8"#),
    }
}

/// A rocket responder that prompts the user to sign in to access the API.
#[derive(rocket::Responder)]
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
