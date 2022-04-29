use http_api_problem::HttpApiProblem;
use http_api_problem::StatusCode;
use rocket::Request;
use rocket_basicauth::unauthorized;

/// Emit HttpApiProblem whenever a Unprocessable Entity error (422) happens
///
/// Rocket emits this error when deserialised JSON is syntactically valid but
/// it's not valid *semantically* (e.g. when passed 0 to a NonZerou32).
/// Display this as error 400 as 422 is Web-Dav specific.
#[rocket::catch(422)]
pub fn unprocessable_entity(req: &Request) -> HttpApiProblem {
    HttpApiProblem::new(StatusCode::BAD_REQUEST)
        .title("Bad Request")
        .detail(format!("{}", req.uri()))
}

/// Provide a set of catchers that catch the most common errors in Rocket: 401, 422
pub fn default_catchers() -> Vec<rocket::Catcher> {
    rocket::catchers![unauthorized, unprocessable_entity,]
}
