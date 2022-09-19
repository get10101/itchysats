use std::*;

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("Could not find any user that fits the specified requirements.")]
    UserNotFound,

    /// This error is thrown when trying to retrieve `Users` but it isn't being managed by the app.
    /// It can be fixed adding `.manage(users)` to the app, where `users` is of type `Users`.
    #[error("UnmanagedStateError: failed retrieving `Users`. You may be missing `.manage(users)` in your app.")]
    UnmanagedState,

    #[error("UnauthenticatedError: Invalid password.")]
    InvalidPassword,

    #[error("{0}")]
    PasswordValidation(String),

    #[error("UnauthenticatedError: The operation failed because the client is not authenticated.")]
    Unauthenticated,

    /// This error occurs when the user does exist, but their password was incorrect.
    #[error("Incorrect password")]
    Unauthorized,

    /// A wrapper around [`argon2::Error`].
    #[error("Argon2ParsingError: {0}")]
    Argon2Parsing(#[from] argon2::Error),

    /// A wrapper around [`serde_json::Error`].
    #[error("SerdeError: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

use self::Error::*;
impl Error {
    fn message(&self) -> String {
        match self {
            Unauthorized | UserNotFound => format!("{}", self),
            #[cfg(debug_assertions)]
            e => format!("{}", e),
            #[allow(unreachable_patterns)]
            _ => "undefined".into(),
        }
    }
}

impl From<Error> for HttpApiProblem {
    fn from(error: Error) -> Self {
        match error {
            UserNotFound => HttpApiProblem::new(StatusCode::NOT_FOUND)
                .title("User not found")
                .detail(format!("{error:#}")),
            UnmanagedState => HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("User state not managed")
                .detail(format!("{error:#}")),
            InvalidPassword => HttpApiProblem::new(StatusCode::UNAUTHORIZED)
                .title("Invalid password")
                .detail(format!("{error:#}")),
            Unauthenticated => HttpApiProblem::new(StatusCode::FORBIDDEN)
                .title("User not authenticated")
                .detail(format!("{error:#}")),
            Unauthorized => HttpApiProblem::new(StatusCode::UNAUTHORIZED)
                .title("User not authorized")
                .detail(format!("{error:#}")),
            Argon2Parsing(e) => HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal server error")
                .detail(format!("{e:#}")),
            Serde(e) => HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Serialization error")
                .detail(format!("{e:#}")),
            Other(e) => HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Internal server error")
                .detail(format!("{e:#}")),
            PasswordValidation(error) => HttpApiProblem::new(StatusCode::BAD_REQUEST)
                .title("Password format error")
                .detail(error),
        }
    }
}

use http_api_problem::HttpApiProblem;
use http_api_problem::StatusCode;
use rocket::http::ContentType;
use rocket::request::Request;
use rocket::response::Responder;
use rocket::response::Response;
use rocket::response::{self};
use serde_json::*;
use std::io::Cursor;

impl<'r> Responder<'r, 'static> for Error {
    fn respond_to(self, _: &'r Request<'_>) -> response::Result<'static> {
        let payload = to_string(&json!({
            "status": "error",
            "message": self.message(),
        }))
        .unwrap();
        Response::build()
            .sized_body(payload.len(), Cursor::new(payload))
            .header(ContentType::new("application", "json"))
            .ok()
    }
}
