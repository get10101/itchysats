PRAGMA foreign_keys=off;

create temporary table orders_backup (
  id,
  uuid,
  trading_pair,
  position,
  initial_price,
  min_quantity,
  max_quantity,
  leverage,
  liquidation_price,
  creation_timestamp_seconds,
  settlement_time_interval_seconds,
  origin,
  oracle_event_id 
);

insert into orders_backup
  select
    id,
    uuid,
    trading_pair,
    position,
    initial_price,
    min_quantity,
    max_quantity,
    leverage,
    liquidation_price,
    creation_timestamp_seconds,
    term_seconds,
    origin,
    oracle_event_id 
  from orders;

drop table orders;

create table orders (
  id                                integer primary key autoincrement,
  uuid                              text unique not null,
  trading_pair                      text        not null,
  position                          text        not null,
  initial_price                     text        not null,
  min_quantity                      text        not null,
  max_quantity                      text        not null,
  leverage                          integer     not null,
  liquidation_price                 text        not null,
  creation_timestamp_seconds        integer     not null,
  settlement_time_interval_seconds  integer     not null,
  origin                            text        not null,
  oracle_event_id                   text        not null
);

insert into orders
  select *
  from orders_backup;
drop table orders_backup;

PRAGMA foreign_keys=on;
