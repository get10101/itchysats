# Itchy Sats

[![Bors enabled](https://bors.tech/images/badge_small.svg)](https://app.bors.tech/repositories/39253)

CFD trading on Bitcoin.

Details coming soon.

## Quickstart

All the components can be started at once by running the following script:

```bash
./start_all.sh
```

Note: Before first run, you need to run `cd frontend; yarn install` command to
ensure that all dependencies get installed.

The script combines the logs from all binaries inside a single terminal so it
might not be ideal for all cases, but it is convenient for quick regression testing.

Pressing `Ctrl + c` once stops all the processes.

The script also enables backtraces by setting `RUST_BACKTRACE=1` env variable.

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

Upon startup the taker daemon will connect to the (hardcoded) maker and retrieve the current order.

Note: The sqlite databases for maker and taker are currently created in the project root.

## Starting the maker and taker frontend

We use a single react project for hosting both the taker and the maker frontends.
However, the development environment still needs to be start twice!
Which frontend to start is configured via the `APP` environment variable.

```bash
cd frontend
yarn install
APP=taker yarn dev
APP=maker yarn dev
```

Bundling the web frontend and serving it from the respective daemon is yet to be configured.
At the moment you will need a browser extension to allow CORS headers like `CORS Everywhere` ([Firefox Extension](https://addons.mozilla.org/en-US/firefox/addon/cors-everywhere/)) to use the frontends.

### Linting

To run eslint, use:

```bash
cd frontend && yarn run eslint
```
