const { readDB, writeDB } = require('./db');

const Product = {
  all: (filters = {}) => {
    const db = readDB();
    let products = db.products;

    if (filters.category && filters.category !== 'All Categories') {
      products = products.filter(p => p.category.toLowerCase() === filters.category.toLowerCase());
    }

    if (filters.search) {
      const q = filters.search.toLowerCase();
      products = products.filter(p => 
        p.title.toLowerCase().includes(q) || 
        p.description.toLowerCase().includes(q)
      );
    }

    return products;
  },

  findById: (id) => {
    const db = readDB();
    return db.products.find(p => p.id === id);
  },

  updateStock: (id, newStock) => {
    const db = readDB();
    const product = db.products.find(p => p.id === id);
    if (!product) throw new Error('Product not found');
    
    const parsedStock = parseInt(newStock, 10);
    if (isNaN(parsedStock) || parsedStock < 0) {
      throw new Error('Invalid stock quantity');
    }
    
    product.stock = parsedStock;
    writeDB(db);
    return product;
  },

  addReview: (id, username, rating, comment) => {
    const db = readDB();
    const product = db.products.find(p => p.id === id);
    if (!product) throw new Error('Product not found');

    const parsedRating = parseFloat(rating);
    if (isNaN(parsedRating) || parsedRating < 1 || parsedRating > 5) {
      throw new Error('Rating must be between 1 and 5');
    }

    if (!product.reviews) {
      product.reviews = [];
    }

    const newReview = {
      id: 'r_' + Math.random().toString(36).substr(2, 9),
      user: username || 'Anonymous',
      rating: parsedRating,
      comment: comment || '',
      date: new Date().toISOString().split('T')[0]
    };

    product.reviews.push(newReview);

    // Recalculate average rating to 1 decimal place
    const totalRating = product.reviews.reduce((sum, r) => sum + r.rating, 0);
    product.rating = Math.round((totalRating / product.reviews.length) * 10) / 10;

    writeDB(db);
    return product;
  }
};

module.exports = Product;
