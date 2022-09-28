use crate::auth::rand_string;
use crate::error::Error;
use anyhow::Result;
use argon2::verify_encoded;
use rocket::http::Status;
use rocket::request::FromRequest;
use rocket::request::Outcome;
use rocket::Request;
use serde::Deserialize;
use serde::Serialize;

/// A request guard that can be included in handler definitions to enforce authentication.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq, Clone, Hash, PartialOrd, Ord)]
pub struct User {
    pub id: u32,
    #[serde(skip_serializing)]
    pub password: String,
    pub auth_key: String,
    pub first_login: bool,
}

#[rocket::async_trait]
impl<'r> FromRequest<'r> for User {
    type Error = Error;
    async fn from_request(request: &'r Request<'_>) -> Outcome<User, Self::Error> {
        use rocket::outcome::Outcome::*;
        let guard = request.guard().await;
        tracing::info!("Guard: {guard}");
        let auth: crate::auth::Auth = match guard {
            Success(auth) => auth,
            Failure(x) => return Failure(x),
            Forward(x) => return Forward(x),
        };
        if let Ok(Some(user)) = auth.get_user().await {
            Success(user)
        } else {
            Failure((Status::Unauthorized, Error::Unauthorized))
        }
    }
}

impl User {
    /// Change users password
    pub fn set_password(&mut self, new: &str) -> Result<()> {
        let password = create_password(new)?;
        self.password = password;
        Ok(())
    }
}

pub fn create_password(password: &str) -> Result<String> {
    let password = password.as_bytes();
    let salt = rand_string(10);
    let config = argon2::Config::default();
    let hash = argon2::hash_encoded(password, salt.as_bytes(), &config)?;
    Ok(hash)
}

pub fn verify_password(encoded_password: &str, plain_password: &str) -> Result<bool> {
    let verified = verify_encoded(encoded_password, plain_password.as_bytes())
        .map_err(Error::Argon2Parsing)?;
    Ok(verified)
}

#[cfg(test)]
mod tests {
    use crate::user::create_password;
    use crate::user::verify_password;

    #[test]
    fn test_create_password() {
        let plain_password = "weareallsatoshi";
        let encoded_password = create_password(plain_password).unwrap();
        verify_password(encoded_password.as_str(), plain_password).unwrap();
    }
}
