const crypto = require('crypto');
const { readDB, writeDB } = require('./db');

function hashPassword(password) {
  return crypto.createHash('sha256').update(password).digest('hex');
}

const User = {
  create: (email, password, name) => {
    const db = readDB();
    if (db.users.find(u => u.email.toLowerCase() === email.toLowerCase())) {
      throw new Error('User already exists');
    }
    const newUser = {
      id: 'u_' + Math.random().toString(36).substr(2, 9),
      email: email.toLowerCase(),
      passwordHash: hashPassword(password),
      name: name,
      wishlist: []
    };
    db.users.push(newUser);
    writeDB(db);
    
    // Return safe user object (no password hash)
    const { passwordHash, ...safeUser } = newUser;
    return safeUser;
  },

  findByEmail: (email) => {
    const db = readDB();
    return db.users.find(u => u.email.toLowerCase() === email.toLowerCase());
  },

  findById: (id) => {
    const db = readDB();
    return db.users.find(u => u.id === id);
  },

  verifyPassword: (user, password) => {
    return user.passwordHash === hashPassword(password);
  },

  addToWishlist: (userId, productId) => {
    const db = readDB();
    const userIndex = db.users.findIndex(u => u.id === userId);
    if (userIndex === -1) throw new Error('User not found');
    
    if (!db.users[userIndex].wishlist) {
      db.users[userIndex].wishlist = [];
    }
    
    if (!db.users[userIndex].wishlist.includes(productId)) {
      db.users[userIndex].wishlist.push(productId);
      writeDB(db);
    }
    return db.users[userIndex].wishlist;
  },

  removeFromWishlist: (userId, productId) => {
    const db = readDB();
    const userIndex = db.users.findIndex(u => u.id === userId);
    if (userIndex === -1) throw new Error('User not found');
    
    if (!db.users[userIndex].wishlist) {
      db.users[userIndex].wishlist = [];
    }
    
    db.users[userIndex].wishlist = db.users[userIndex].wishlist.filter(id => id !== productId);
    writeDB(db);
    return db.users[userIndex].wishlist;
  },

  getWishlist: (userId) => {
    const db = readDB();
    const user = db.users.find(u => u.id === userId);
    if (!user) return [];
    const wishlistIds = user.wishlist || [];
    return db.products.filter(p => wishlistIds.includes(p.id));
  }
};

module.exports = User;
