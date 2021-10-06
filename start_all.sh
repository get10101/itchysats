#!/usr/bin/env bash

export RUST_BACKTRACE=1

# A simple command to spin up the complete package, ie. both daemons and frontends.
# A single 'ctrl+c' stops all processes.
(trap 'kill 0' SIGINT; cargo run --bin maker & cargo run --bin taker & APP=maker yarn --cwd=./frontend dev  & APP=taker yarn --cwd=./frontend dev)
