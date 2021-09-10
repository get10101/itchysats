-- todo: Decimal is had to deserialize as number so we use text
create table if not exists offers
(
    id                 integer primary key autoincrement,
    uuid               text unique not null,
    trading_pair       text        not null,
    position           text        not null,
    initial_price      text        not null,
    min_quantity       text        not null,
    max_quantity       text        not null,
    leverage           integer     not null,
    liquidation_price  text        not null,
    creation_timestamp text        not null,
    term               text        not null
);

create unique index if not exists offers_uuid
    on offers (uuid);

create table if not exists cfds
(
    id           integer primary key autoincrement,
    offer_id     integer unique not null,
    offer_uuid   text unique    not null,
    quantity_usd text           not null,

    foreign key (offer_id) references offers (id)
);

create unique index if not exists cfd_offer_uuid
    on cfds (offer_uuid);

create table if not exists cfd_states
(
    id                   integer primary key autoincrement,
    cfd_id               integer not null,
    state                text    not null,
    foreign key (cfd_id) references cfds (id)
);
