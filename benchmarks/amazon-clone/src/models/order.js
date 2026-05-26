const { readDB, writeDB } = require('./db');
const Product = require('./product');
const Cart = require('./cart');

function generateOrderId() {
  const chars = 'ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789';
  let id = '';
  for (let i = 0; i < 10; i++) {
    id += chars.charAt(Math.floor(Math.random() * chars.length));
  }
  return id;
}

function maskCardNumber(cardNumber) {
  const clean = cardNumber.replace(/\D/g, '');
  if (clean.length < 4) return '****';
  return '****' + clean.slice(-4);
}

const Order = {
  getById: (userId, orderId) => {
    const db = readDB();
    return db.orders.find((order) => order.id === orderId && order.userId === userId) || null;
  },

  create: (userId, orderData) => {
    const {
      email,
      fullName,
      address,
      city,
      state,
      zip,
      cardNumber,
      expiryDate,
      cvv,
      items
    } = orderData;

    // Validation
    if (!email || !email.includes('@')) {
      throw new Error('Valid email address is required');
    }
    if (!fullName || fullName.trim().length < 2) {
      throw new Error('Full name is required (minimum 2 characters)');
    }
    if (!address || address.trim().length < 10) {
      throw new Error('Shipping address is required (minimum 10 characters)');
    }
    if (!city || !city.trim()) {
      throw new Error('City is required');
    }
    if (!state || !state.trim()) {
      throw new Error('State is required');
    }
    if (!zip || !/^\d+$/.test(zip)) {
      throw new Error('ZIP code must be numeric');
    }
    const cleanCard = cardNumber ? cardNumber.replace(/\s/g, '') : '';
    if (!cleanCard || !/^\d{16}$/.test(cleanCard)) {
      throw new Error('Card number must be 16 digits');
    }
    if (!expiryDate || !/^\d{2}\/\d{2}$/.test(expiryDate)) {
      throw new Error('Expiry date must be in MM/YY format');
    }
    if (!cvv || !/^\d{3,4}$/.test(cvv)) {
      throw new Error('CVV must be 3 or 4 digits');
    }
    if (!items || items.length === 0) {
      throw new Error('Cannot checkout with an empty cart');
    }

    // Deduct stock and assemble items list
    const db = readDB();
    const processedItems = [];
    let orderTotal = 0;

    for (const item of items) {
      const product = db.products.find(p => p.id === item.productId);
      if (!product) {
        throw new Error(`Product ${item.productId} not found`);
      }
      if (product.stock < item.quantity) {
        throw new Error(`Insufficient stock for product: ${product.title}`);
      }
      product.stock -= item.quantity;
      
      const itemTotal = product.price * item.quantity;
      orderTotal += itemTotal;

      processedItems.push({
        productId: product.id,
        title: product.title,
        price: product.price,
        quantity: item.quantity,
        total: itemTotal
      });
    }

    const orderId = generateOrderId();
    const estimatedDelivery = new Date();
    estimatedDelivery.setDate(estimatedDelivery.getDate() + 4); // 4 days from today

    const newOrder = {
      id: orderId,
      userId,
      email: email.toLowerCase(),
      fullName,
      address: `${address}, ${city}, ${state} ${zip}`,
      maskedCard: maskCardNumber(cleanCard),
      items: processedItems,
      total: Math.round(orderTotal * 100) / 100,
      status: 'Processing',
      date: new Date().toISOString().split('T')[0],
      estimatedDelivery: estimatedDelivery.toISOString().split('T')[0]
    };

    db.orders.push(newOrder);
    
    // Clear user's cart
    db.carts[userId] = { items: [] };

    writeDB(db);
    return newOrder;
  },

  list: (userId) => {
    const db = readDB();
    return db.orders.filter(o => o.userId === userId);
  }
};

module.exports = Order;
