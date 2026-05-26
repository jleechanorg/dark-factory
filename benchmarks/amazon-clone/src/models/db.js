const fs = require('fs');
const path = require('path');

const dbPath = path.join(__dirname, '../data/db.json');

const defaultDB = {
  products: [],
  users: [],
  carts: {},
  orders: [],
};

function normalizeDB(raw) {
  const data = { ...defaultDB, ...(raw || {}) };
  data.products = Array.isArray(data.products) ? data.products : [];
  data.users = Array.isArray(data.users) ? data.users : [];
  data.orders = Array.isArray(data.orders) ? data.orders : [];
  data.carts = data.carts && typeof data.carts === 'object' ? data.carts : {};
  return data;
}

function readSeed() {
  const seedPath = path.join(__dirname, '../data/db.json');
  try {
    const raw = JSON.parse(fs.readFileSync(seedPath, 'utf8'));
    return normalizeDB(raw);
  } catch {
    return normalizeDB(defaultDB);
  }
}

function readDB() {
  try {
    return normalizeDB(JSON.parse(fs.readFileSync(dbPath, 'utf8')));
  } catch {
    const db = readSeed();
    writeDB(db);
    return db;
  }
}

function writeDB(db) {
  const next = normalizeDB(db);
  fs.mkdirSync(path.dirname(dbPath), { recursive: true });
  fs.writeFileSync(dbPath, `${JSON.stringify(next, null, 2)}\n`);
}

function resetDB() {
  const seed = readSeed();
  const fresh = normalizeDB(seed);
  writeDB(fresh);
  return fresh;
}

function seedFromFile(filePath) {
  const next = normalizeDB(JSON.parse(fs.readFileSync(filePath, 'utf8')));
  writeDB(next);
  return next;
}

function getPath() {
  return dbPath;
}

module.exports = {
  readDB,
  writeDB,
  resetDB,
  seedFromFile,
  getPath,
};
