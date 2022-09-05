use anyhow::Result;
use rocket::async_trait;
use rocket::form::Form;
use rocket::get;
use rocket::post;
use rocket::response::Redirect;
use rocket::routes;
use rocket_cookie_auth::auth::Auth;
use rocket_cookie_auth::error::Error;
use rocket_cookie_auth::forms::ChangePassword;
use rocket_cookie_auth::forms::Login;
use rocket_cookie_auth::user::User;
use rocket_cookie_auth::users::Users;
use rocket_cookie_auth::Database;
use rocket_cookie_auth::NO_AUTH_KEY_SET;
use rocket_dyn_templates::Template;
use serde_json::json;
use std::path::PathBuf;

pub struct InMemoryState {
    pub users: Vec<User>,
}

#[get("/")]
async fn index(user: Option<User>) -> Template {
    tracing::info!("User: {user:?}");
    if let Some(user) = &user {
        if user.first_login {
            return Template::render("change-password", json!({}));
        }
    }
    Template::render("index", json!({ "user": user }))
}

#[post("/login", data = "<form>")]
async fn post_login(auth: Auth<'_>, form: Form<Login>) -> Result<Redirect, Error> {
    auth.login(&form).await?;
    Ok(Redirect::to("/"))
}

#[post("/change-password", data = "<form>")]
async fn change_password(
    mut user: User,
    auth: Auth<'_>,
    form: Form<ChangePassword>,
) -> Result<Redirect, Error> {
    form.is_secure()?;
    user.set_password(&form.password)?;
    auth.users.update_user(user).await?;
    Ok(Redirect::to("/"))
}

#[get("/logout")]
fn logout(auth: Auth<'_>) -> Result<Redirect, Error> {
    auth.logout()?;
    Ok(Redirect::to("/"))
}

#[rocket::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().init();

    let sql_file = "./example-rocket-cookie-auth.sql".to_string();
    let db = Connection::new(sqlite_db::connect(PathBuf::from(sql_file), true).await?);

    let users = Users::new(Box::new(db));

    let mission_success = rocket::build()
        .mount("/", routes![index, post_login, logout, change_password])
        .manage(users)
        .attach(Template::fairing())
        .launch()
        .await?;

    tracing::trace!(?mission_success, "Rocket has landed");
    Ok(())
}

struct Connection {
    inner: sqlite_db::Connection,
}

impl Connection {
    fn new(db: sqlite_db::Connection) -> Self {
        Self { inner: db }
    }
}

#[async_trait]
impl Database for Connection {
    async fn load_user(&self) -> Result<Option<User>> {
        let users = self.inner.clone().load_user().await?;
        Ok(users.map(|user| User {
            id: user.id,
            password: user.password,
            auth_key: NO_AUTH_KEY_SET.to_string(),
            first_login: user.first_login,
        }))
    }

    async fn update_password(&self, password: String) -> Result<()> {
        self.inner.clone().update_password(password).await?;
        Ok(())
    }
}
