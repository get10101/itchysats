#[macro_use]
extern crate rocket;
use crate::user::User;
use anyhow::Result;
use rocket::async_trait;

pub mod auth;
pub mod error;
pub mod forms;
mod session;
pub mod user;
pub mod users;

/// Temporary value if no authentication key is set
pub const NO_AUTH_KEY_SET: &str = "NONE";

#[async_trait]
pub trait Database: Send + Sync {
    async fn load_user(&self) -> Result<Option<User>>;
    async fn update_password(&self, password: String) -> Result<()>;
}
