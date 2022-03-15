-- Add migration script here
DROP TABLE cfd_states;
DROP TABLE cfds;
CREATE TABLE IF NOT EXISTS cfds (
    id SERIAL PRIMARY KEY,
    order_id integer UNIQUE NOT NULL,
    order_uuid text UNIQUE NOT NULL,
    quantity_usd text NOT NULL,
    counterparty text NOT NULL,
    FOREIGN KEY (order_id) REFERENCES orders (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS cfd_order_uuid ON cfds (order_uuid);
CREATE TABLE IF NOT EXISTS cfd_states (
    id SERIAL PRIMARY KEY,
    cfd_id integer NOT NULL,
    state text NOT NULL,
    FOREIGN KEY (cfd_id) REFERENCES cfds (id)
);
