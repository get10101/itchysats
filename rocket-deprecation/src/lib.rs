use rocket::http::uri;
use rocket::http::Header;
use rocket::response::Responder;
use rocket::Request;
use time::OffsetDateTime;

/// A rocket responder for deprecating endpoints using the `Deprecation` header defined in https://datatracker.ietf.org/doc/html/draft-ietf-httpapi-deprecation-header.
pub struct Deprecation<TInner> {
    /// The inner responder that creates the actual response.
    inner: TInner,

    /// The `Deprecation` header that will be set in the response.
    deprecation: Header<'static>,

    /// Links added to the response.
    links: Vec<Header<'static>>,
}

impl<'r, TInner> Responder<'r, 'static> for Deprecation<TInner>
where
    TInner: Responder<'r, 'static>,
{
    fn respond_to(self, request: &'r Request<'_>) -> rocket::response::Result<'static> {
        let mut response = self.inner.respond_to(request)?;

        response.set_header(self.deprecation);

        for link in self.links {
            response.adjoin_header(link);
        }

        Ok(response)
    }
}

impl<TInner> Deprecation<TInner> {
    /// Wraps another response, adding the `Deprecation: true` header.
    pub fn new(response: TInner) -> Self {
        Self {
            inner: response,
            deprecation: Header::new(HEADER_NAME, "true"),
            links: vec![],
        }
    }

    /// Wraps another response, adding the `Deprecation` header set to the given timestamp.
    pub fn with_timestamp(response: TInner, timestamp: OffsetDateTime) -> Self {
        let timestamp = timestamp
            .format(&time::format_description::well_known::Rfc2822)
            .expect("date to format");

        Self {
            inner: response,
            deprecation: Header::new(HEADER_NAME, timestamp),
            links: vec![],
        }
    }

    /// Adds a `Link` header with `rel="deprecation"` to the response.
    ///
    /// For more information, see https://datatracker.ietf.org/doc/html/draft-ietf-httpapi-deprecation-header#section-3.
    pub fn with_deprecation_link(mut self, link: uri::Absolute) -> Self {
        self.links.push(Header::new(
            "Link",
            format!(r#"<{link}>; rel="deprecation""#),
        ));

        self
    }

    // TODO: Add support for other link relations: https://datatracker.ietf.org/doc/html/draft-dalal-deprecation-header-03#section-4
}

const HEADER_NAME: &str = "Deprecation";

#[cfg(test)]
mod tests {
    use super::*;
    use rocket::local::blocking::Client;
    use rocket::uri;
    use rocket::Build;
    use rocket::Rocket;

    #[test]
    fn route_has_deprecation_header_true() {
        let client = Client::tracked(rocket()).unwrap();

        let response = client.get("/test").dispatch();

        assert_eq!(response.headers().get_one("Deprecation"), Some("true"));
    }

    #[test]
    fn route_has_deprecation_header_with_timestamp() {
        let client = Client::tracked(rocket()).unwrap();

        let response = client.get("/test2").dispatch();

        assert_eq!(
            response.headers().get_one("Deprecation"),
            Some("Thu, 01 Jan 1970 00:00:00 +0000")
        );
    }

    #[test]
    fn route_has_deprecation_header_with_link() {
        let client = Client::tracked(rocket()).unwrap();

        let response = client.get("/test3").dispatch();

        assert_eq!(
            response.headers().get_one("Link"),
            Some(r#"<http://example.com/deprecated-resources/test2>; rel="deprecation""#)
        );
    }

    #[rocket::get("/test")]
    async fn deprecated_true() -> Deprecation<()> {
        Deprecation::new(())
    }

    #[rocket::get("/test2")]
    async fn deprecated_with_time() -> Deprecation<()> {
        Deprecation::with_timestamp((), OffsetDateTime::UNIX_EPOCH)
    }

    #[rocket::get("/test3")]
    async fn deprecated_with_link() -> Deprecation<()> {
        Deprecation::new(())
            .with_deprecation_link(uri!("http://example.com/deprecated-resources/test2"))
    }

    /// Constructs a Rocket instance for testing.
    fn rocket() -> Rocket<Build> {
        rocket::build().mount(
            "/",
            rocket::routes![deprecated_true, deprecated_with_time, deprecated_with_link],
        )
    }
}
