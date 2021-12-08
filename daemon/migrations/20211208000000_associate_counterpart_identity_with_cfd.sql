-- Add migration script here
drop table cfd_states;
drop table cfds;

create table if not exists cfds
(
    id                    integer primary key autoincrement,
    order_id              integer unique not null,
    order_uuid            text unique    not null,
    quantity_usd          text           not null,
    counterparty text           not null,
    foreign key (order_id) references orders (id)
);

create unique index if not exists cfd_order_uuid
    on cfds (order_uuid);

create table if not exists cfd_states
(
    id     integer primary key autoincrement,
    cfd_id integer not null,
    state  text    not null,
    foreign key (cfd_id) references cfds (id)
);

