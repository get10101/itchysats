drop table cfd_states;
drop table cfds;
drop table orders;

create table if not exists cfds
(
    id                               integer primary key autoincrement,
    uuid                             text unique not null,
    trading_pair                     text        not null,
    position                         text        not null,
    initial_price                    text        not null,
    leverage                         integer     not null,
    liquidation_price                text        not null,
    creation_timestamp_seconds       integer     not null,
    settlement_time_interval_seconds integer     not null,
    origin                           text        not null,
    oracle_event_id                  text        not null,
    fee_rate                         integer     not null,
    quantity_usd                     text        not null,
    counterparty                     text        not null
);

create unique index if not exists cfd_uuid on cfds (uuid);

create table if not exists cfd_states
(
    id     integer primary key autoincrement,
    cfd_id integer not null,
    state  text    not null,
    foreign key (cfd_id) references cfds (id)
);
