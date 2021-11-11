#!/usr/bin/env bash

export RUST_BACKTRACE=1

# A simple command to spin up the complete package, ie. both daemons and frontends.
# A single 'ctrl+c' stops all processes.
# The maker-id is generated from the makers seed found in daemon/util/testnet_seeds/maker_seed
(trap 'kill 0' SIGINT; cargo run --bin maker -- testnet & cargo run --bin taker -- --maker-id 10d4ba2ac3f7a22da4009d813ff1bc3f404dfe2cc93a32bedf1512aa9951c95e testnet -- & yarn --cwd=./maker-frontend dev  & yarn --cwd=./taker-frontend dev)
