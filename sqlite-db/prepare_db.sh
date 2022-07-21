#!/bin/bash

TEMPDB=${PWD}/tempdb

# make sure to fail early in case something goes wrong
set -e

# create temporary DB
DATABASE_URL=sqlite:$TEMPDB cargo sqlx database create
# make sure we remove the tempdb when exiting even if one of the following commands fails
trap 'rm -f $TEMPDB' EXIT

# run the migration scripts to create the tables
DATABASE_URL=sqlite:$TEMPDB cargo sqlx migrate run

# prepare the sqlx-data.json rust mappings
DATABASE_URL=sqlite:$TEMPDB SQLX_OFFLINE=true cargo sqlx prepare -- --tests
