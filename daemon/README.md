# Maker & Taker Daemon

Daemon that enables the frontend.
The frontend is just a very thin display layer, the daemon does all the heavy lifting and calculations.

## Database

We use an `sqlite` database managed by `sqlx`.

To make `sqlx` handle the rust types correctly you have to generate `sqlx-data.json` file upon every query change.
So, if you develop on the DB and change queries you will have to update the `sqlx` rust mappings like this:

```bash
# crated temporary DB
DATABASE_URL=sqlite:tempdb cargo sqlx database create

# run the migration scripts to create the tables
DATABASE_URL=sqlite:tempdb cargo sqlx migrate run

# prepare the sqlx-data.json rust mappings
DATABASE_URL=sqlite:./daemon/tempdb cargo sqlx prepare -- --bin taker
```

Currently the database for taker and maker is the same.
The `taker` binary is used as an example to run the `prepare` command above, but it is irrelevant if you run it for taker or maker.
The `tempdb` created can be deleted, it should not be checked into the repo.
You can keep it around and just run the `prepare` statement multiple times when working on the database.
