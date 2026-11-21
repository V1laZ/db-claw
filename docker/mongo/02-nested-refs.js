// Nested ObjectId references (for inferred reference navigation):
//   users.address.cityId -> cities, orders.items[].productId -> products.
// Safe to run again.
const shop = db.getSiblingDB("shop");
const cityNames = ["Prague", "Brno", "Berlin", "Vienna", "NYC"];
const countries = { Prague: "CZ", Brno: "CZ", Berlin: "DE", Vienna: "AT", NYC: "US" };
for (const name of cityNames) {
  shop.cities.updateOne({ name }, { $setOnInsert: { name, country: countries[name] } }, { upsert: true });
}
for (const c of shop.cities.find().toArray()) {
  shop.users.updateMany({ "address.city": c.name }, { $set: { "address.cityId": c._id } });
}
for (const p of shop.products.find({}, { sku: 1 }).toArray()) {
  shop.orders.updateMany(
    { "items.sku": p.sku },
    { $set: { "items.$[i].productId": p._id } },
    { arrayFilters: [{ "i.sku": p.sku }] },
  );
}
