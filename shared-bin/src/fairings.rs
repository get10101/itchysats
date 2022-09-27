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

            tracing::info!(target: "http", endpoint = %http_endpoint, "HTTP interface is ready");
        })
    })
}

/// Attach this fairing to enable logging Rocket HTTP requests
pub fn log_requests() -> impl Fairing {
    AdHoc::on_response("Log status code for request", |request, response| {
        Box::pin(async move {
            let method = request.method();
            let path = request.uri().path();
            let status = response.status();

            // Prevents logging binary blobs (e.g. seed) in logs
            let path = if path.starts_with("/api") || path.starts_with("/assets") {
                path.as_str()
            } else {
                "<hidden>"
            };
            tracing::debug!(target: "http", %method, %path, %status, "Handled request");
        })
    })
}

/// Attach this fairing to enable loading the UI in the system default browser
///
/// Passing `true` opens browser at launch, passing `false` logs the link to
/// open it manually.
pub fn ui_browser_launch(open_browser: bool) -> impl Fairing {
    AdHoc::on_liftoff("ui browser launch", move |rocket| {
        Box::pin(async move {
            let http_endpoint = format!(
                "http://{}:{}",
                rocket.config().address,
                rocket.config().port
            );

            if !open_browser {
                tracing::info!(
                    "Running in headless mode. The UI can be accessed at {http_endpoint}"
                );
                return;
            }

            match webbrowser::open(http_endpoint.as_str()) {
                Ok(()) => {
                    tracing::info!("The user interface was opened in your default browser");
                }
                Err(e) => {
                    tracing::debug!(
                        "Could not open user interface at {} in default browser because {e:#}",
                        http_endpoint
                    );
                }
            }
        })
    })
}
