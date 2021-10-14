-- todo: Decimal is had to deserialize as number so we use text
create table if not exists orders
(
    id                              integer primary key autoincrement,
    uuid                            text unique not null,
    trading_pair                    text        not null,
    position                        text        not null,
    initial_price                   text        not null,
    min_quantity                    text        not null,
    max_quantity                    text        not null,
    leverage                        integer     not null,
    liquidation_price               text        not null,
    creation_timestamp_seconds      integer     not null,
    creation_timestamp_nanoseconds  integer     not null,
    term_seconds                    integer     not null,
    term_nanoseconds                integer     not null,
    origin                          text        not null,
    oracle_event_id                 text        not null
);

create unique index if not exists orders_uuid
    on orders (uuid);

create table if not exists cfds
(
    id           integer primary key autoincrement,
    order_id     integer unique not null,
    order_uuid   text unique    not null,
    quantity_usd text           not null,

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
