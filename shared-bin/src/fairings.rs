use rocket::fairing::AdHoc;
use rocket::fairing::Fairing;

/// Attach this fairing to enable logging Rocket launch
pub fn log_launch() -> impl Fairing {
    AdHoc::on_liftoff("Log launch", |rocket| {
        Box::pin(async move {
            let http_endpoint = format!(
                "http://{}:{}",
                rocket.config().address,
                rocket.config().port
            );

            tracing::info!(endpoint = %http_endpoint, "HTTP interface is ready");
        })
    })
}

/// Attach this fairing to enable logging Rocket HTTP requests
pub fn log_requests() -> impl Fairing {
    AdHoc::on_request("Rocket HTTP request", |request, _data| {
        Box::pin(async move {
            tracing::debug!(%request, "HTTP");
        })
    })
}
