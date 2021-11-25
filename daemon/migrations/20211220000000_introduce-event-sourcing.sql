drop table cfd_states;
drop table cfds;

create table if not exists cfds
(
    id                              integer primary key autoincrement,
    uuid                            text unique not null,
    position                        text        not null,
    initial_price                   text        not null,
    leverage                        integer     not null,
    settlement_time_interval_hours  integer     not null,
    quantity_usd                    text        not null,
    counterparty_network_identity   text        not null,
    role                            text        not null
);

create unique index if not exists cfds_uuid
    on cfds (uuid);

create table if not exists events
(
    id         integer primary key autoincrement,
    cfd_id     integer not null,
    name       text not null,
    data       text not null,
    created_at text not null,

    foreign key (cfd_id) references cfds (id)
)
