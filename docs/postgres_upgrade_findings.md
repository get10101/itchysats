# Sqlite to Postgres upgrade findings

## Model crate

We have a macro that implements type conversion for sqlite. I have replaced it with conversions to postgres. It should
be trivial to reintroduce the sqlite conversion.

## Daemon crate

I replaced sqlite with postgres and was able to make and take a cfd. This has broken the tests because we do not have
an automated postgres setup. We can get them working by replacing the concrete postgres implementation
`sqlx::pool::Pool<PgPool>` with `sqlx::pool::Pool<T>`. This will allow us to support the taker on sqlite and the maker
on postgres. We will need to have separate `daemon::db::connect` functions for postgres and sqlite and our
`daemon::db::migrate` function will need to take in the migration folder path as a parameter as sqlite and postgres
have different migrations.

## SQL incompatibilities

There were two postgres incompatibilities with our sqlite crate.

1. Postgres does not support auto-increment; I replaced it with the postgres equivalent: `SERIAL`.
2. When altering tables in our migrations, we need to specify the type.

I am not sure how relevant finding 2 is as it may just be better to condense the migrations into one that represents
the current schema since we are starting a new database.

## Postgres setup

I used nix-os to run postgres as a service on my machine. Unfortunately no one is using nix-os. Nix-os generates a
[systemd service file](postgres.service) which may be useful.

If you are on mac, you can get a dev setup going quickly with https://postgresapp.com/

For dev purposes you do not need to configure any fancy authentication.

```
export DATABASE_URL="postgres://postgres@localhost/itchysats"
```

## Authentication

I did not explore this deeply as the solution depends on our cloud architecture. Do we use managed postgres or do run it
ourselves on the same (or different) vm.

There are numerous authentication schemes available which can be configured using the `pg_hba.conf` file.
Relevant docs: https://www.postgresql.org/docs/current/auth-pg-hba-conf.html
