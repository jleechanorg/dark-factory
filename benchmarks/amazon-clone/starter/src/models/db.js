const fs = require("fs");
const path = require("path");

const DB_FILE = path.join(__dirname, "..", "data", "db.json");

function clone(data) {
  return JSON.parse(JSON.stringify(data));
}

function readDB() {
  const raw = fs.readFileSync(DB_FILE, "utf-8");
  return clone(JSON.parse(raw));
}

function writeDB(data) {
  fs.writeFileSync(DB_FILE, JSON.stringify(data, null, 2), "utf-8");
}

function resetDB() {
  const fresh = {
    users: [],
    products: [],
    carts: {},
    orders: [],
    sessions: {},
    sellerProfiles: {},
    moderationEvents: [],
    notificationPreferences: {},
  };
  writeDB(fresh);
  return fresh;
}

module.exports = {
  readDB,
  writeDB,
  resetDB,
};
