# rust-embed-rocket

Provides interoperability between rust-embed and rocket.

This crate was split out whilst tearing apart the original `daemon` crate.
A better design would be to create a `rocket-embed` crate that also allows us to remove the code duplication between the routes of the binary.
Currently, we have to re-declare the embed-routes in every frontend.
