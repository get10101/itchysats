# Project Hermes

CFD trading on Bitcoin.

Details coming soon.

## Starting the maker and taker daemon

The maker and taker frontend depend on the respective daemon running.

At the moment the maker daemon has to be started first:

```bash
cargo run --bin maker
```

Once the maker is started you can start the taker:

```bash
cargo run --bin taker
```

Upon startup the taker daemon will connect to the (hardcoded) maker and retrieve the current offer.

Note: The sqlite databases for maker and taker are currently created in the project root.

## Starting the maker and taker frontend

We use a single react project for hosting both the taker and the maker frontends.

To start it in development mode:

```bash
cd frontend && yarn dev
```

- To access maker: [Maker](http://localhost:3000/maker)
- To access taker: [Taker](http://localhost:3000/taker)

Bundling the web frontend and serving it from the respective daemon is yet to be configured.
At the moment you will need a browser extension to allow CORS headers like `CORS Everywhere` ([Firefox Extension](https://addons.mozilla.org/en-US/firefox/addon/cors-everywhere/)) to use the frontends.

### Linting

To run eslint, use:

```bash
cd frontend && yarn run eslint
```
