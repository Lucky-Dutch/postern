CREATE TABLE products (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    name text NOT NULL,
    category text NOT NULL,
    price numeric(10, 2) NOT NULL CHECK (price >= 0),
    in_stock boolean NOT NULL DEFAULT true,
    sku text UNIQUE,
    created_at timestamptz NOT NULL DEFAULT now()
);
COMMENT ON TABLE products IS 'Disposable fixtures for UI smoke tests';
INSERT INTO products (name, category, price, in_stock, sku) VALUES
('Ceramic pour-over', 'Coffee', 38.00, true, 'CF-1001'),
('Daily notebook', 'Stationery', 18.50, true, 'ST-2001'),
('Linen desk mat', 'Workspace', 42.00, true, 'WS-3001'),
('Glass carafe', 'Coffee', 29.00, false, 'CF-1002'),
('Mechanical pencil', 'Stationery', 24.00, true, 'ST-2002'),
('Studio lamp', 'Workspace', 129.00, true, 'WS-3002'),
('Espresso cup set', 'Coffee', 34.50, true, 'CF-1003'),
('Weekly planner', 'Stationery', 22.00, false, 'ST-2003'),
('Oak monitor stand', 'Workspace', 89.00, true, 'WS-3003'),
('Coffee scale', 'Coffee', 59.00, true, 'CF-1004'),
('Fountain pen', 'Stationery', 65.00, true, 'ST-2004'),
('Cable organizer', 'Workspace', 16.00, true, NULL);
CREATE TABLE customers (id bigint PRIMARY KEY, name text, email text);
CREATE TABLE orders (id bigint PRIMARY KEY, customer_id bigint REFERENCES customers(id), total numeric);
CREATE TABLE order_items (order_id bigint REFERENCES orders(id), product_id bigint REFERENCES products(id), quantity integer, PRIMARY KEY(order_id, product_id));
INSERT INTO customers VALUES (1, 'Ada Lovelace', 'ada@example.com'), (2, 'Grace Hopper', 'grace@example.com');
INSERT INTO orders VALUES (100, 1, 76.00), (101, 2, 129.00), (102, 1, 18.50);
INSERT INTO order_items VALUES (100, 1, 2), (101, 6, 1), (102, 2, 1);
CREATE VIEW available_products AS SELECT * FROM products WHERE in_stock;
-- Index advisor fixtures: a redundant prefix index and an exact duplicate.
CREATE INDEX products_category ON products (category);
CREATE INDEX products_category_price ON products (category, price);
CREATE INDEX products_name ON products (name);
CREATE INDEX products_name_copy ON products (name);
CREATE FUNCTION product_count(wanted_category text) RETURNS bigint
LANGUAGE sql STABLE AS $$
    SELECT count(*) FROM products WHERE category = wanted_category
$$;
CREATE TABLE measurements (taken_on date NOT NULL, reading numeric) PARTITION BY RANGE (taken_on);
CREATE TABLE measurements_2025 PARTITION OF measurements FOR VALUES FROM ('2025-01-01') TO ('2026-01-01');
CREATE TABLE measurements_2026 PARTITION OF measurements FOR VALUES FROM ('2026-01-01') TO ('2027-01-01');
CREATE TABLE unicode_samples (id integer PRIMARY KEY, language text NOT NULL, value text NOT NULL);
INSERT INTO unicode_samples VALUES
(1, 'Japanese', '抹茶ラーメン'),
(2, 'Chinese', '牛奶饮料'),
(3, 'Korean', '우유 라면'),
(4, 'Arabic', 'حليب'),
(5, 'Hindi', 'दूध'),
(6, 'Thai', 'นม'),
(7, 'Russian', 'Яшкино вафли'),
(8, 'Polish', 'Zażółć gęślą jaźń');
ANALYZE;
-- A table with dead rows that is read by sequential scans.
CREATE TABLE page_views (id bigint PRIMARY KEY, path text NOT NULL, viewed_at timestamptz NOT NULL)
WITH (autovacuum_enabled = false);
INSERT INTO page_views
SELECT i, '/products/' || (i % 40), timestamptz '2026-01-01 00:00+00' + i * interval '1 minute'
FROM generate_series(1, 60000) i;
DELETE FROM page_views WHERE id % 3 = 0;
-- Query statistics; the test cluster preloads pg_stat_statements.
CREATE EXTENSION IF NOT EXISTS pg_stat_statements;
SELECT pg_stat_statements_reset() IS NOT NULL AS statistics_reset;
SELECT path, count(*) FROM page_views WHERE viewed_at >= '2026-01-15' GROUP BY path ORDER BY 2 DESC LIMIT 10;
SELECT path, count(*) FROM page_views WHERE viewed_at >= '2026-01-20' GROUP BY path ORDER BY 2 DESC LIMIT 10;
SELECT path, count(*) FROM page_views WHERE viewed_at >= '2026-01-25' GROUP BY path ORDER BY 2 DESC LIMIT 10;
SELECT id, viewed_at FROM page_views WHERE path = '/products/7' ORDER BY viewed_at DESC LIMIT 20;
SELECT id, viewed_at FROM page_views WHERE path = '/products/8' ORDER BY viewed_at DESC LIMIT 20;
SELECT id, viewed_at FROM page_views WHERE path = '/products/9' ORDER BY viewed_at DESC LIMIT 20;
SELECT c.name, sum(o.total) FROM customers c JOIN orders o ON o.customer_id = c.id GROUP BY c.name;
SELECT c.name, sum(o.total) FROM customers c JOIN orders o ON o.customer_id = c.id GROUP BY c.name;
SELECT count(*) FROM generate_series(1, 500000) g WHERE g % 7 = 0;
SELECT pg_sleep(0.25);
CREATE DATABASE analytics;
CREATE DATABASE project_dev;
CREATE DATABASE project_test;
\connect analytics
CREATE TABLE events (id bigint PRIMARY KEY, name text, event_count integer);
INSERT INTO events VALUES (1, 'Page viewed', 128), (2, 'Account created', 24), (3, 'Order completed', 12);
\connect postgres
