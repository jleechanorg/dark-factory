const { readDB, writeDB } = require('./db');
const Product = require('./product');

const Cart = {
  get: (userId) => {
    const db = readDB();
    if (!db.carts[userId]) {
      db.carts[userId] = { items: [] };
      writeDB(db);
    }
    return db.carts[userId];
  },

  addItem: (userId, productId, quantity = 1) => {
    const db = readDB();
    if (!db.carts[userId]) {
      db.carts[userId] = { items: [] };
    }

    const cart = db.carts[userId];
    const product = Product.findById(productId);
    if (!product) throw new Error('Product not found');

    const qty = parseInt(quantity, 10);
    if (isNaN(qty) || qty <= 0) throw new Error('Quantity must be greater than zero');

    const existingItem = cart.items.find(item => item.productId === productId);
    if (existingItem) {
      existingItem.quantity += qty;
    } else {
      // Obeys soft limit of 50 unique items
      if (cart.items.length >= 50) {
        throw new Error('Cart soft limit reached (max 50 unique items)');
      }
      cart.items.push({
        productId,
        quantity: qty,
        priceAtAdd: product.price
      });
    }

    writeDB(db);
    return cart;
  },

  updateItem: (userId, productId, quantity) => {
    const db = readDB();
    const cart = db.carts[userId];
    if (!cart) throw new Error('Cart not found');

    const qty = parseInt(quantity, 10);
    if (isNaN(qty) || qty < 0) throw new Error('Quantity must be zero or positive');

    if (qty === 0) {
      cart.items = cart.items.filter(item => item.productId !== productId);
    } else {
      const item = cart.items.find(item => item.productId === productId);
      if (!item) throw new Error('Item not found in cart');
      item.quantity = qty;
    }

    writeDB(db);
    return cart;
  },

  removeItem: (userId, productId) => {
    const db = readDB();
    const cart = db.carts[userId];
    if (!cart) throw new Error('Cart not found');

    cart.items = cart.items.filter(item => item.productId !== productId);
    writeDB(db);
    return cart;
  },

  clear: (userId) => {
    const db = readDB();
    db.carts[userId] = { items: [] };
    writeDB(db);
    return db.carts[userId];
  }
};

module.exports = Cart;
