INSERT INTO customers (email, name, country, vip, balance, tags, profile, created_at)
SELECT
    'user' || g || '@example.' || (ARRAY['com','cz','de','org'])[1 + g % 4],
    (ARRAY['Jana','Petr','Lukas','Eva','John','Maria','Tomas','Anna','Karel','Sofia'])[1 + g % 10] || ' ' ||
    (ARRAY['Novak','Svoboda','Smith','Müller','Dvorak','Garcia','Horak','Kral'])[1 + g % 8],
    (ARRAY['CZ','DE','US','SK','AT', NULL])[1 + g % 6],
    g % 7 = 0,
    round((random() * 5000)::numeric, 2),
    CASE WHEN g % 3 = 0 THEN ARRAY['newsletter','beta'] WHEN g % 3 = 1 THEN ARRAY['newsletter'] ELSE '{}' END,
    CASE WHEN g % 5 = 0 THEN NULL ELSE jsonb_build_object('age', 18 + g % 50, 'city', (ARRAY['Prague','Brno','Berlin','Vienna','NYC'])[1 + g % 5], 'prefs', jsonb_build_object('theme', CASE WHEN g % 2 = 0 THEN 'dark' ELSE 'light' END)) END,
    now() - (g || ' hours')::interval
FROM generate_series(1, 5000) g;

INSERT INTO products (sku, title, price, stock, weight_kg, released)
SELECT 'SKU-' || lpad(g::text, 5, '0'),
       (ARRAY['Mechanical keyboard','USB-C cable','Monitor 27"','Desk lamp','Office chair','Notebook','Headphones','Webcam'])[1 + g % 8] || ' #' || g,
       round((5 + random() * 995)::numeric, 2), (random() * 500)::int, round((random() * 20)::numeric, 3)::float8,
       date '2020-01-01' + (g * 3)
FROM generate_series(1, 400) g;

INSERT INTO orders (customer_id, status, total, note, placed_at)
SELECT 1 + (g * 7) % 5000,
       (ARRAY['pending','paid','shipped','cancelled']::order_status[])[1 + g % 4],
       round((10 + random() * 2000)::numeric, 2),
       CASE WHEN g % 10 = 0 THEN 'Please deliver after 5pm' END,
       now() - (g || ' minutes')::interval
FROM generate_series(1, 20000) g;

INSERT INTO order_items (order_id, product_id, quantity, unit_price)
SELECT o.id, p.id, 1 + (o.id % 3)::int, p.price
FROM orders o
CROSS JOIN LATERAL (SELECT id, price FROM products ORDER BY md5(o.id::text || id::text) LIMIT 2) p
WHERE o.id <= 5000;

INSERT INTO audit_log (actor, action, payload)
SELECT 'admin', (ARRAY['login','export','delete'])[1 + g % 3], json_build_object('n', g)
FROM generate_series(1, 50) g;

INSERT INTO analytics.events (kind, at, meta)
SELECT (ARRAY['view','click','purchase'])[1 + g % 3], now() - (g || ' seconds')::interval, jsonb_build_object('session', md5(g::text))
FROM generate_series(1, 100000) g;

ANALYZE;
