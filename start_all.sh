#!/usr/bin/env bash

export RUST_BACKTRACE=1

# A simple command to spin up the complete package, ie. both daemons and frontends.
# A single 'ctrl+c' stops all processes.
# The maker-id is generated from the makers seed found in daemon/testnet/maker_seed
(trap 'kill 0' SIGINT; cargo dev-maker & cargo dev-taker -- & yarn --cwd=./maker-frontend dev  & yarn --cwd=./taker-frontend dev)
