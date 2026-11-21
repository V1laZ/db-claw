-- Sample schema exercising many Postgres types.
CREATE TYPE order_status AS ENUM ('pending', 'paid', 'shipped', 'cancelled');

CREATE TABLE customers (
    id          serial PRIMARY KEY,
    email       text NOT NULL UNIQUE,
    name        text NOT NULL,
    country     char(2),
    vip         boolean NOT NULL DEFAULT false,
    balance     numeric(12, 2) NOT NULL DEFAULT 0,
    tags        text[] NOT NULL DEFAULT '{}',
    profile     jsonb,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE products (
    id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    sku         varchar(32) NOT NULL UNIQUE,
    title       text NOT NULL,
    price       numeric(10, 2) NOT NULL,
    stock       integer NOT NULL DEFAULT 0,
    weight_kg   double precision,
    released    date
);

CREATE TABLE orders (
    id          bigserial PRIMARY KEY,
    customer_id integer NOT NULL REFERENCES customers(id) ON DELETE CASCADE,
    status      order_status NOT NULL DEFAULT 'pending',
    total       numeric(12, 2) NOT NULL,
    note        text,
    placed_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE order_items (
    order_id    bigint NOT NULL REFERENCES orders(id) ON DELETE CASCADE,
    product_id  uuid NOT NULL REFERENCES products(id),
    quantity    integer NOT NULL CHECK (quantity > 0),
    unit_price  numeric(10, 2) NOT NULL,
    PRIMARY KEY (order_id, product_id)
);

-- A table without a primary key (edits fall back to ctid).
CREATE TABLE audit_log (
    at      timestamptz NOT NULL DEFAULT now(),
    actor   text,
    action  text NOT NULL,
    payload json
);

CREATE SCHEMA analytics;
CREATE VIEW analytics.revenue_by_country AS
    SELECT c.country, count(o.id) AS orders, sum(o.total) AS revenue
    FROM customers c JOIN orders o ON o.customer_id = c.id
    GROUP BY c.country;

CREATE TABLE analytics.events (
    id      bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    kind    text NOT NULL,
    at      timestamptz NOT NULL,
    meta    jsonb
);
