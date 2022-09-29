#![allow(clippy::let_unit_value)] // see: https://github.com/SergioBenitez/Rocket/issues/2211

use anyhow::Result;
use http_api_problem::HttpApiProblem;
use http_api_problem::StatusCode;
use rocket::form::Form;
use rocket::serde::json::Json;
use rocket_cookie_auth::auth::Auth;
use rocket_cookie_auth::forms::ChangePassword;
use rocket_cookie_auth::forms::Login;
use rocket_cookie_auth::user::User;
use serde::Serialize;
use tracing::instrument;

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    daemon_version: String,
}

#[rocket::get("/alive")]
pub fn get_health_check() {}

#[rocket::get("/version")]
#[instrument(name = "GET /version")]
pub async fn get_version() -> Json<HealthCheck> {
    Json(HealthCheck {
        daemon_version: daemon::version(),
    })
}

#[rocket::post("/change-password", data = "<form>")]
pub async fn change_password(
    mut user: User,
    auth: Auth<'_>,
    form: Form<ChangePassword>,
) -> Result<(), HttpApiProblem> {
    form.clone().is_secure().map_err(|error| {
        tracing::error!("{error:#}");
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Invalid password format")
            .detail(format!("{error:#}"))
    })?;

    user.set_password(&form.password).map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not set password")
            .detail(format!("{error:#}"))
    })?;
    auth.users.update_user(user).await.map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could update user password")
            .detail(format!("{error:#}"))
    })?;
    Ok(())
}

#[derive(Serialize, Copy, Clone)]
pub struct Authenticated {
    authenticated: bool,
    first_login: bool,
}

#[rocket::get("/am-I-authenticated")]
pub async fn is_authenticated(auth: Auth<'_>) -> Result<Json<Authenticated>, HttpApiProblem> {
    let authenticated = auth.is_auth().map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not check authentication")
            .detail(format!("{error:#}"))
    })?;
    let first_login = auth
        .get_user()
        .await
        .map_err(|error| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Could not get user from session")
                .detail(format!("{error:#}"))
        })?
        .map(|user| user.first_login)
        .unwrap_or_default();
    Ok(Json(Authenticated {
        authenticated,
        first_login,
    }))
}

#[rocket::get("/logout")]
pub fn logout(auth: Auth<'_>) -> Result<(), HttpApiProblem> {
    auth.logout().map_err(|error| {
        HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
            .title("Could not logout")
            .detail(format!("{error:#}"))
    })?;
    Ok(())
}

/// Login a user. If successful a cookie will be return
///
/// E.g.
/// curl -d "password=password" -X POST http://localhost:8000/api/login
#[rocket::post("/login", data = "<form>")]
pub async fn post_login(auth: Auth<'_>, form: Form<Login>) -> Result<Json<User>, HttpApiProblem> {
    let user = auth.login(&form).await?;
    Ok(Json(user))
}

// TODO: Use non-cookie auth for /metrics endpoint as Prometheus does not
// support cookie-auth (for now, leave unauthenticated)
#[rocket::get("/metrics")]
#[instrument(name = "GET /metrics", skip_all, err)]
pub async fn get_metrics<'r>() -> Result<String, HttpApiProblem> {
    let metrics = prometheus::TextEncoder::new()
        .encode_to_string(&prometheus::gather())
        .map_err(|e| {
            HttpApiProblem::new(StatusCode::INTERNAL_SERVER_ERROR)
                .title("Failed to encode metrics")
                .detail(e.to_string())
        })?;

    Ok(metrics)
}
