# Itchy Sats

[![Bors enabled](https://bors.tech/images/badge_small.svg)](https://app.bors.tech/repositories/39253)

CFD trading on Bitcoin.

Details coming soon.

## Quickstart

All the components can be started at once by running the following script:

```bash
./start_all.sh
```

Note: Before first run, you need to run `cd maker-frontend; yarn install; cd../taker-frontend; yarn install` command to ensure that all dependencies get
installed.

The script combines the logs from all binaries inside a single terminal so it
might not be ideal for all cases, but it is convenient for quick regression testing.

Pressing `Ctrl + c` once stops all the processes.

The script also enables backtraces by setting `RUST_BACKTRACE=1` env variable.

## Starting the maker and taker daemon

A working example of starting both daemons with all the required command-line parameters can be found
[here](https://github.com/itchysats/itchysats/blob/master/start_all.sh#L8)

The maker and taker frontend depend on the respective daemon running.

## Starting the maker and taker frontend

We use a separate react projects for hosting taker and maker frontends.

### Building the frontends

The latest version of the built frontends will be embedded by `cargo` inside
their respective daemons and automatically served when the daemon starts.
Embedded frontend is served on ports `8000` and `8001` by default.

This means that it is highly recommended to build the frontend _before_ the daemons.

#### Taker

```bash
cd taker-frontend
yarn install
yarn build
```

#### Maker

```bash
cd maker-frontend
yarn install
yarn build
```

### Developing frontend code

If hot-reloading of the app is required, frontend can be started in development mode.
Development frontend is served on ports `3000` and `3001` by default.

#### Taker

```bash
cd taker-frontend
yarn install
yarn dev
```

#### Maker

```bash
cd maker-frontend
yarn install
yarn dev
```

### Linting

To run eslint, use:

```bash
cd maker-frontend && yarn run eslint
cd taker-frontend && yarn run eslint
```
