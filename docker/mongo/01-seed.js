// Seed data for the "shop" database.
const shop = db.getSiblingDB("shop");
const names = ["Jana", "Petr", "Lukas", "Eva", "John", "Maria", "Tomas", "Anna", "Karel", "Sofia"];
const cities = ["Prague", "Brno", "Berlin", "Vienna", "NYC"];

const users = [];
for (let i = 1; i <= 5000; i++) {
  const u = {
    email: `user${i}@example.com`,
    name: names[i % names.length],
    age: 18 + (i % 50),
    active: i % 4 !== 0,
    balance: NumberDecimal(((i * 37) % 5000 + 0.5).toString()),
    tags: i % 3 === 0 ? ["newsletter", "beta"] : ["newsletter"],
    address: { city: cities[i % cities.length], zip: String(10000 + i) },
    createdAt: new Date(Date.now() - i * 3600 * 1000),
  };
  if (i % 5 === 0) u.nickname = `nick${i}`;          // sparse field
  if (i % 11 === 0) u.loginCount = NumberLong(i * 10); // Int64
  users.push(u);
}
shop.users.insertMany(users);
shop.users.createIndex({ email: 1 }, { unique: true });

const products = [];
for (let i = 1; i <= 400; i++) {
  products.push({ sku: `SKU-${String(i).padStart(5, "0")}`, title: `Product ${i}`, price: 5 + (i * 13) % 995, stock: (i * 7) % 500, dims: { w: i % 40, h: i % 30 } });
}
shop.products.insertMany(products);

const userIds = shop.users.find({}, { _id: 1 }).limit(500).toArray().map((u) => u._id);
const orders = [];
for (let i = 1; i <= 20000; i++) {
  orders.push({
    userId: userIds[i % userIds.length],
    status: ["pending", "paid", "shipped", "cancelled"][i % 4],
    total: Math.round(Math.random() * 200000) / 100,
    items: [{ sku: `SKU-${String(1 + (i % 400)).padStart(5, "0")}`, qty: 1 + (i % 3) }],
    placedAt: new Date(Date.now() - i * 60 * 1000),
  });
}
shop.orders.insertMany(orders);

shop.createCollection("empty_collection");
