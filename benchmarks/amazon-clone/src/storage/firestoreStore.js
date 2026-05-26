const admin = require('firebase-admin');
const crypto = require('crypto');
const {
  FIRESTORE_EMULATOR_HOST,
  FIREBASE_PROJECT_ID,
} = require('../config/constants');

const COLLECTIONS = {
  USERS: 'users',
  SESSIONS: 'sessions',
  PRODUCTS: 'products',
  INVENTORY: 'inventory',
  CARTS: 'carts',
  WISHLISTS: 'wishlists',
  ADDRESSES: 'addresses',
  ORDERS: 'orders',
  REVIEWS: 'reviews',
  COUPONS: 'coupons',
  NOTIFICATIONS: 'notifications',
  MODERATION: 'moderationEvents',
  SELLER_PROFILES: 'sellerProfiles',
  METRICS: 'metricsSnapshots',
};

function initDb() {
  if (!admin.apps.length) {
    admin.initializeApp({ projectId: FIREBASE_PROJECT_ID });
  }

  const db = admin.firestore();
  if (FIRESTORE_EMULATOR_HOST) {
    db.settings({
      host: FIRESTORE_EMULATOR_HOST,
      ssl: false,
    });
  }

  return db;
}

const db = initDb();
const serverTimestamps = {
  now: admin.firestore.FieldValue.serverTimestamp,
};

function deterministicId(prefix, value) {
  const normalized = `${value}`;
  const digest = crypto.createHash('sha1').update(normalized).digest('hex').slice(0, 18);
  return `${prefix}_${digest}`;
}

function randomId(prefix, length = 16) {
  const raw = `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}`;
  const digest = crypto.createHash('sha1').update(raw).digest('hex').slice(0, length);
  return `${prefix}_${digest}`;
}

function cents(value) {
  const numeric = typeof value === 'number'
    ? value
    : Number.parseFloat(`${value}`.replace(/,/g, '').replace(/\$/g, '').trim());

  if (Number.isNaN(numeric)) {
    throw new Error('Invalid money value');
  }

  if (Number.isInteger(numeric)) {
    return numeric;
  }

  return Math.round(numeric * 100);
}

function moneyFromCents(valueInCents) {
  if (typeof valueInCents !== 'number' || !Number.isFinite(valueInCents)) {
    return 0;
  }
  return Number((valueInCents / 100).toFixed(2));
}

function sanitizeObject(obj) {
  if (!obj || typeof obj !== 'object') {
    return obj;
  }
  const out = Array.isArray(obj) ? [] : {};
  for (const [key, value] of Object.entries(obj)) {
    if (value === undefined) {
      continue;
    }
    if (key === 'passwordHash' || key === 'password') {
      continue;
    }
    if (Array.isArray(value) || value instanceof Date) {
      out[key] = value;
    } else if (value && typeof value === 'object' && !admin.firestore.Timestamp.isTimestamp(value)) {
      out[key] = sanitizeObject(value);
    } else {
      out[key] = value;
    }
  }
  return out;
}

function withTimestamps(data) {
  return {
    ...data,
    createdAt: serverTimestamps.now(),
    updatedAt: serverTimestamps.now(),
  };
}

function now() {
  return new Date().toISOString();
}

function toDocData(doc) {
  const base = doc.data() || {};
  return {
    id: doc.id,
    ...base,
  };
}

function listFromCollection(snap) {
  return snap.docs.map((d) => toDocData(d));
}

async function clearCollection(name) {
  const snapshot = await db.collection(name).get();
  if (snapshot.empty) {
    return;
  }

  const batch = db.batch();
  snapshot.forEach((doc) => batch.delete(doc.ref));
  await batch.commit();
}

async function clearCollections(collectionNames = []) {
  for (const name of collectionNames) {
    await clearCollection(name);
  }
}

const usersRepo = {
  async create(userPayload) {
    const email = `${userPayload.email || ''}`.trim().toLowerCase();
    if (!email) {
      throw new Error('Email required');
    }

    const existing = await usersRepo.findByEmail(email);
    if (existing) {
      throw new Error('User already exists');
    }

    const id = userPayload.id || deterministicId('user', email);
    const user = {
      id,
      email,
      name: `${userPayload.name || ''}`.trim(),
      role: userPayload.role || 'shopper',
      passwordHash: userPayload.passwordHash,
      defaultAddressId: userPayload.defaultAddressId || null,
      notificationPreferences: {
        email: false,
        marketing: false,
        orderStatus: true,
      },
      sellerProfileId: userPayload.sellerProfileId || null,
      createdAt: now(),
      updatedAt: now(),
    };

    await db.collection(COLLECTIONS.USERS).doc(id).set({
      ...user,
      updatedAt: serverTimestamps.now(),
    });

    return sanitizeObject(user);
  },

  async findById(id) {
    const snap = await db.collection(COLLECTIONS.USERS).doc(id).get();
    if (!snap.exists) {
      return null;
    }
    return toDocData(snap);
  },

  async findByEmail(email) {
    const norm = `${email || ''}`.trim().toLowerCase();
    const snapshot = await db
      .collection(COLLECTIONS.USERS)
      .where('email', '==', norm)
      .limit(1)
      .get();
    if (snapshot.empty) {
      return null;
    }
    return toDocData(snapshot.docs[0]);
  },

  async setDefaultAddress(userId, addressId) {
    await db.collection(COLLECTIONS.USERS).doc(userId).update({
      defaultAddressId: addressId,
      updatedAt: serverTimestamps.now(),
    });
    return usersRepo.findById(userId);
  },

  async all() {
    const snapshot = await db.collection(COLLECTIONS.USERS).get();
    return listFromCollection(snapshot);
  },

  async updateRole(userId, role) {
    await db.collection(COLLECTIONS.USERS).doc(userId).update({
      role,
      updatedAt: serverTimestamps.now(),
    });
    return usersRepo.findById(userId);
  },
};

const sessionsRepo = {
  async create({ userId = null, role = 'guest', email = null, name = null }) {
    const id = randomId('sess');
    const token = id;
    const nowDate = now();
    const session = {
      id: token,
      userId,
      role,
      email,
      name,
      isGuest: !userId,
      createdAt: nowDate,
      updatedAt: nowDate,
      expiresAt: new Date(Date.now() + 1000 * 60 * 60 * 24 * 14).toISOString(),
    };
    await db.collection(COLLECTIONS.SESSIONS).doc(token).set({
      ...session,
      updatedAt: serverTimestamps.now(),
    });
    return session;
  },

  async findById(id) {
    const doc = await db.collection(COLLECTIONS.SESSIONS).doc(id).get();
    if (!doc.exists) {
      return null;
    }
    const session = toDocData(doc);
    if (session.expiresAt && new Date(session.expiresAt).getTime() < Date.now()) {
      await sessionsRepo.destroy(id);
      return null;
    }
    return session;
  },

  async touch(id) {
    await db.collection(COLLECTIONS.SESSIONS).doc(id).update({
      updatedAt: serverTimestamps.now(),
      expiresAt: new Date(Date.now() + 1000 * 60 * 60 * 24 * 14).toISOString(),
    });
  },

  async destroy(id) {
    await db.collection(COLLECTIONS.SESSIONS).doc(id).delete();
  },
};

const productsRepo = {
  async list(filters = {}) {
    const snapshot = await db.collection(COLLECTIONS.PRODUCTS).get();
    const products = listFromCollection(snapshot);
    const normalized = products.map((product) => {
      const stock = product.stockOnHand != null
        ? product.stockOnHand
        : 0;
      return {
        ...product,
        stock,
        price: moneyFromCents(product.priceCents || 0),
      };
    });

    return normalized.filter((product) => {
      if (filters && filters.department && filters.department !== 'All Departments' && filters.department !== 'All Categories') {
        if ((product.department || '').toLowerCase() !== `${filters.department}`.toLowerCase()) {
          return false;
        }
      }

      if (filters && filters.seller && filters.seller.toLowerCase() !== `${product.sellerId || ''}`.toLowerCase()) {
        return false;
      }

      if (filters && filters.tags && filters.tags.length) {
        const tags = new Set([...(product.tags || []), ...(product.tagNames || [])].map((tag) => `${tag}`.toLowerCase()));
        const requested = new Set(filters.tags.map((tag) => `${tag}`.toLowerCase()));
        const hasIntersection = [...requested].some((tag) => tags.has(tag));
        if (!hasIntersection) {
          return false;
        }
      }

      const search = `${filters.search || ''}`.trim().toLowerCase();
      if (search) {
        const haystack = [
          product.title,
          product.brand,
          product.department,
          product.description,
          ...(product.tags || []),
          ...(product.searchTerms || []),
        ].filter(Boolean).join(' ').toLowerCase();
        if (!haystack.includes(search)) {
          return false;
        }
      }

      if (filters.minRating != null && (product.ratingAverage || 0) < filters.minRating) {
        return false;
      }

      if (filters.minReviewCount != null && (product.reviewCount || 0) < filters.minReviewCount) {
        return false;
      }

      if (filters.fastDelivery) {
        if ((product.deliveryPromiseDays || 999) > 2) {
          return false;
        }
      }

      if (filters.stockState === 'in-stock' && product.stock <= 0) {
        return false;
      }

      if (filters.stockState === 'out-of-stock' && product.stock > 0) {
        return false;
      }

      if (filters.discountOnly && !(product.listPriceCents > product.priceCents)) {
        return false;
      }

      if (filters.maxPrice != null && product.price > filters.maxPrice) {
        return false;
      }

      if (filters.minPrice != null && product.price < filters.minPrice) {
        return false;
      }

      if (filters.suspended !== undefined && Boolean(filters.suspended) !== Boolean(product.suspended)) {
        return false;
      }

      return filters.includeArchived || product.active !== false;
    }).sort((left, right) => {
      const sortBy = `${filters.sort || 'relevance'}`;
      if (sortBy === 'price_asc') {
        return (left.price || 0) - (right.price || 0);
      }
      if (sortBy === 'price_desc') {
        return (right.price || 0) - (left.price || 0);
      }
      if (sortBy === 'rating') {
        return (right.ratingAverage || 0) - (left.ratingAverage || 0);
      }
      if (sortBy === 'newest') {
        return String(right.createdAt || '').localeCompare(String(left.createdAt || ''));
      }
      if (sortBy === 'delivery') {
        return (left.deliveryPromiseDays || 0) - (right.deliveryPromiseDays || 0);
      }
      if (sortBy === 'reviews') {
        return (right.reviewCount || 0) - (left.reviewCount || 0);
      }
      return 0;
    });
  },

  async getById(id) {
    const doc = await db.collection(COLLECTIONS.PRODUCTS).doc(id).get();
    if (!doc.exists) {
      return null;
    }
    const data = toDocData(doc);
    return {
      ...data,
      stock: data.stockOnHand != null ? data.stockOnHand : 0,
      price: moneyFromCents(data.priceCents || 0),
      listPrice: moneyFromCents(data.listPriceCents || 0),
    };
  },

  async create(payload) {
    const id = payload.id || randomId('prod');
    const nowValue = now();
    const newProduct = {
      id,
      sellerId: payload.sellerId,
      title: payload.title,
      brand: payload.brand || 'Amazon Vendor',
      department: payload.department,
      category: payload.department,
      description: payload.description,
      priceCents: cents(payload.priceCents || payload.price),
      listPriceCents: cents(payload.listPriceCents || payload.listPrice || payload.price || 0),
      imageUrls: payload.imageUrls || [],
      ratingAverage: payload.ratingAverage != null ? payload.ratingAverage : 0,
      reviewCount: 0,
      tags: payload.tags || [],
      active: true,
      suspended: false,
      searchTerms: payload.searchTerms || [],
      deliveryPromiseDays: payload.deliveryPromiseDays || 3,
      createdAt: nowValue,
      updatedAt: nowValue,
    };

    await db.collection(COLLECTIONS.PRODUCTS).doc(id).set(newProduct);
    await inventoryRepo.create({
      productId: id,
      stockOnHand: payload.stockOnHand != null ? payload.stockOnHand : 0,
      lowStockThreshold: payload.lowStockThreshold || 5,
      reservedCount: 0,
    });
    return productsRepo.getById(id);
  },

  async patch(id, patch) {
    const updates = {
      ...patch,
      updatedAt: now(),
    };
    if (patch.priceCents != null) updates.priceCents = cents(patch.priceCents);
    if (patch.price != null) updates.priceCents = cents(patch.price);
    if (patch.listPrice != null || patch.listPriceCents != null) {
      updates.listPriceCents = cents(patch.listPriceCents != null ? patch.listPriceCents : patch.listPrice);
    }
    await db.collection(COLLECTIONS.PRODUCTS).doc(id).update(updates);
    return productsRepo.getById(id);
  },

  async archive(id) {
    await db.collection(COLLECTIONS.PRODUCTS).doc(id).update({
      active: false,
      suspended: true,
      archivedAt: now(),
      updatedAt: now(),
    });
    return productsRepo.getById(id);
  },

  async allIds() {
    const snapshot = await db.collection(COLLECTIONS.PRODUCTS).get();
    return snapshot.docs.map((doc) => doc.id);
  },
};

const inventoryRepo = {
  async create(payload) {
    const id = payload.id || randomId('inv');
    await db.collection(COLLECTIONS.INVENTORY).doc(id).set({
      id,
      productId: payload.productId,
      stockOnHand: payload.stockOnHand || 0,
      lowStockThreshold: payload.lowStockThreshold || 0,
      reservedCount: payload.reservedCount || 0,
      updatedAt: now(),
    });
    return db.collection(COLLECTIONS.INVENTORY).doc(id).id;
  },

  async getByProduct(productId) {
    const snapshot = await db.collection(COLLECTIONS.INVENTORY)
      .where('productId', '==', productId)
      .limit(1)
      .get();
    if (snapshot.empty) {
      return null;
    }
    return toDocData(snapshot.docs[0]);
  },

  async setStockByProduct(productId, stockOnHand, lowStockThreshold = null) {
    const current = await inventoryRepo.getByProduct(productId);
    if (!current) {
      const id = randomId('inv');
      await db.collection(COLLECTIONS.INVENTORY).doc(id).set({
        id,
        productId,
        stockOnHand,
        lowStockThreshold,
        reservedCount: 0,
        updatedAt: now(),
      });
      return db.collection(COLLECTIONS.INVENTORY).doc(id);
    }
    const update = {
      stockOnHand,
      updatedAt: now(),
    };
    if (lowStockThreshold != null) {
      update.lowStockThreshold = lowStockThreshold;
    }
    await db.collection(COLLECTIONS.INVENTORY).doc(current.id).update(update);
    return inventoryRepo.getByProduct(productId);
  },

  async restock(productId, delta) {
    const current = await inventoryRepo.getByProduct(productId);
    if (!current) {
      throw new Error('Inventory record missing');
    }
    const stockOnHand = (current.stockOnHand || 0) + delta;
    if (stockOnHand < 0) {
      throw new Error('Cannot restock to negative stock');
    }
    await db.collection(COLLECTIONS.INVENTORY).doc(current.id).update({
      stockOnHand,
      updatedAt: now(),
    });
    await productsRepo.patch(productId, { stockOnHand });
    return stockOnHand;
  },

  async reserve(productId, quantity, tx = null) {
    const snapshot = await db.collection(COLLECTIONS.INVENTORY)
      .where('productId', '==', productId)
      .limit(1)
      .get();

    if (snapshot.empty) {
      throw new Error('Inventory record not found');
    }

    const inventoryRef = snapshot.docs[0].ref;
    const inventory = snapshot.docs[0].data();
    const available = Number(inventory.stockOnHand || 0) - Number(inventory.reservedCount || 0);

    if (!Number.isFinite(quantity) || quantity <= 0) {
      throw new Error('Invalid reservation quantity');
    }
    if (available < quantity) {
      throw new Error('Insufficient stock');
    }

    const newReservedCount = Number(inventory.reservedCount || 0) + Number(quantity);
    if (tx) {
      tx.update(inventoryRef, {
        reservedCount: newReservedCount,
        updatedAt: now(),
      });
    } else {
      await inventoryRef.update({
        reservedCount: newReservedCount,
        updatedAt: now(),
      });
    }

    return {
      inventoryId: snapshot.docs[0].id,
      stockOnHand: inventory.stockOnHand,
      reservedCount: newReservedCount,
    };
  },

  async commitOrder(productId, quantity, tx = null) {
    const snapshot = await db.collection(COLLECTIONS.INVENTORY)
      .where('productId', '==', productId)
      .limit(1)
      .get();

    if (snapshot.empty) {
      throw new Error('Inventory record not found');
    }

    const inv = snapshot.docs[0].data();
    const stockOnHand = Number(inv.stockOnHand || 0);
    if (stockOnHand < quantity) {
      throw new Error('Insufficient stock at checkout');
    }

    const reservedCount = Number(inv.reservedCount || 0);
    const newReserved = Math.max(0, reservedCount - quantity);
    const newStock = stockOnHand - quantity;
    const ref = snapshot.docs[0].ref;

    if (tx) {
      tx.update(ref, {
        stockOnHand: newStock,
        reservedCount: newReserved,
        updatedAt: now(),
      });
    } else {
      await ref.update({
        stockOnHand: newStock,
        reservedCount: newReserved,
        updatedAt: now(),
      });
    }

    await db.collection(COLLECTIONS.PRODUCTS).doc(productId).update({
      stockOnHand: newStock,
      updatedAt: now(),
    });

    return {
      stockOnHand: newStock,
      reservedCount: newReserved,
    };
  },
};

const cartsRepo = {
  async get(userId) {
    const id = userId;
    const snap = await db.collection(COLLECTIONS.CARTS).doc(id).get();
    if (snap.exists) {
      return toDocData(snap);
    }

    const cart = {
      id,
      userId,
      items: [],
      savedForLater: [],
      couponCode: null,
      totalsPreview: {
        subtotalCents: 0,
        discountCents: 0,
        taxCents: 0,
        shippingCents: 0,
        totalCents: 0,
      },
      updatedAt: now(),
      createdAt: now(),
    };
    await db.collection(COLLECTIONS.CARTS).doc(id).set(cart);
    return cart;
  },

  async set(userId, cart) {
    await db.collection(COLLECTIONS.CARTS).doc(userId).set({
      ...cart,
      userId,
      updatedAt: now(),
      createdAt: cart.createdAt || now(),
    });
    return cartsRepo.get(userId);
  },

  async addItem(userId, productId, quantity) {
    const numericQuantity = Number(quantity);
    if (!Number.isInteger(numericQuantity) || numericQuantity <= 0) {
      throw new Error('Quantity must be a positive integer');
    }

    const cart = await cartsRepo.get(userId);
    const product = await productsRepo.getById(productId);
    if (!product) {
      throw new Error('Product not found');
    }

    const existing = (cart.items || []).find((item) => item.productId === productId);
    const itemList = [...(cart.items || [])];
    if (existing) {
      existing.quantity += numericQuantity;
    } else {
      itemList.push({
        productId,
        quantity: numericQuantity,
        addedAt: now(),
        priceAtAddCents: product.priceCents,
      });
    }

    const limited = itemList.map((item) => ({
      ...item,
      quantity: Math.min(item.quantity, 20),
    }));

    await cartsRepo.set(userId, {
      ...cart,
      items: limited,
    });

    return cartsRepo.get(userId);
  },

  async setItemQuantity(userId, productId, quantity) {
    const numeric = Number(quantity);
    if (!Number.isInteger(numeric) || numeric < 0) {
      throw new Error('Quantity must be an integer >= 0');
    }

    const cart = await cartsRepo.get(userId);
    let items = [...(cart.items || [])];
    if (numeric === 0) {
      items = items.filter((item) => item.productId !== productId);
    } else {
      const found = items.find((item) => item.productId === productId);
      if (!found) {
        throw new Error('Item not found in cart');
      }
      found.quantity = numeric;
    }
    await cartsRepo.set(userId, { ...cart, items });
    return cartsRepo.get(userId);
  },

  async removeItem(userId, productId) {
    const cart = await cartsRepo.get(userId);
    const items = (cart.items || []).filter((item) => item.productId !== productId);
    await cartsRepo.set(userId, { ...cart, items });
    return cartsRepo.get(userId);
  },

  async clear(userId) {
    const cart = await cartsRepo.get(userId);
    await cartsRepo.set(userId, {
      ...cart,
      items: [],
      savedForLater: [],
      couponCode: null,
      totalsPreview: {
        subtotalCents: 0,
        discountCents: 0,
        shippingCents: 0,
        taxCents: 0,
        totalCents: 0,
      },
    });
    return cartsRepo.get(userId);
  },

  async applyCoupon(userId, couponCode) {
    const cart = await cartsRepo.get(userId);
    const code = `${couponCode || ''}`.trim().toUpperCase();
    const coupon = await couponsRepo.getByCode(code);
    if (!coupon || !coupon.active) {
      throw new Error('Invalid coupon');
    }
    if (coupon.expiresAt && new Date(coupon.expiresAt).getTime() < Date.now()) {
      throw new Error('Coupon expired');
    }

    await cartsRepo.set(userId, {
      ...cart,
      couponCode: code,
      totalsPreview: {
        ...cart.totalsPreview,
        couponCode: code,
      },
    });

    return cartsRepo.get(userId);
  },

  async saveForLater(userId, productId) {
    const cart = await cartsRepo.get(userId);
    const has = (cart.items || []).some((item) => item.productId === productId);
    if (!has) {
      throw new Error('Product not found in cart');
    }
    const savedSet = new Set(cart.savedForLater || []);
    savedSet.add(productId);
    const items = (cart.items || []).filter((item) => item.productId !== productId);

    await cartsRepo.set(userId, {
      ...cart,
      items,
      savedForLater: [...savedSet],
    });
    return cartsRepo.get(userId);
  },
};

const wishlistsRepo = {
  async getByUser(userId) {
    const snap = await db.collection(COLLECTIONS.WISHLISTS).doc(userId).get();
    if (snap.exists) {
      return toDocData(snap);
    }

    const base = {
      id: userId,
      userId,
      productIds: [],
      createdAt: now(),
      updatedAt: now(),
    };
    await db.collection(COLLECTIONS.WISHLISTS).doc(userId).set(base);
    return base;
  },

  async add(userId, productId) {
    const product = await productsRepo.getById(productId);
    if (!product) {
      throw new Error('Product not found');
    }
    const wishlist = await wishlistsRepo.getByUser(userId);
    const ids = new Set(wishlist.productIds || []);
    ids.add(productId);
    await db.collection(COLLECTIONS.WISHLISTS).doc(userId).update({
      productIds: [...ids],
      updatedAt: serverTimestamps.now(),
    });
    return wishlistsRepo.getByUser(userId);
  },

  async remove(userId, productId) {
    const wishlist = await wishlistsRepo.getByUser(userId);
    const productIds = (wishlist.productIds || []).filter((id) => id !== productId);
    await db.collection(COLLECTIONS.WISHLISTS).doc(userId).update({
      productIds,
      updatedAt: serverTimestamps.now(),
    });
    return wishlistsRepo.getByUser(userId);
  },

  async asProducts(userId) {
    const wishlist = await wishlistsRepo.getByUser(userId);
    const productIds = wishlist.productIds || [];
    const products = [];
    for (const productId of productIds) {
      const product = await productsRepo.getById(productId);
      if (product) {
        products.push(product);
      }
    }
    return products;
  },
};

const addressesRepo = {
  async listByUser(userId) {
    const snapshot = await db
      .collection(COLLECTIONS.ADDRESSES)
      .where('userId', '==', userId)
      .get();
    return listFromCollection(snapshot);
  },

  async getById(id) {
    const snap = await db.collection(COLLECTIONS.ADDRESSES).doc(id).get();
    if (!snap.exists) {
      return null;
    }
    return toDocData(snap);
  },

  async create(userId, payload) {
    const id = payload.id || randomId('addr');
    const item = {
      id,
      userId,
      recipient: `${payload.recipient || ''}`.trim(),
      street: `${payload.street || ''}`.trim(),
      unit: `${payload.unit || ''}`.trim(),
      city: `${payload.city || ''}`.trim(),
      region: `${payload.region || ''}`.trim(),
      postalCode: `${payload.postalCode || ''}`.trim(),
      country: `${payload.country || 'USA'}`.trim(),
      phone: `${payload.phone || ''}`.trim(),
      isDefault: Boolean(payload.isDefault),
      createdAt: now(),
      updatedAt: now(),
    };
    await db.collection(COLLECTIONS.ADDRESSES).doc(id).set(item);
    if (item.isDefault) {
      await usersRepo.setDefaultAddress(userId, id);
    }
    return item;
  },

  async update(id, userId, payload) {
    const existing = await addressesRepo.getById(id);
    if (!existing || existing.userId !== userId) {
      throw new Error('Address not found');
    }

    const update = {
      ...payload,
      updatedAt: now(),
      id,
    };

    await db.collection(COLLECTIONS.ADDRESSES).doc(id).update(update);

    if (payload.isDefault) {
      await usersRepo.setDefaultAddress(userId, id);
    }

    return addressesRepo.getById(id);
  },

  async destroy(id, userId) {
    const existing = await addressesRepo.getById(id);
    if (!existing || existing.userId !== userId) {
      throw new Error('Address not found');
    }
    await db.collection(COLLECTIONS.ADDRESSES).doc(id).delete();
    return { id };
  },

  async makeDefault(userId, id) {
    const existing = await addressesRepo.getById(id);
    if (!existing || existing.userId !== userId) {
      throw new Error('Address not found');
    }
    await usersRepo.setDefaultAddress(userId, id);
    return addressesRepo.getById(id);
  },
};

const reviewsRepo = {
  async list(filters = {}) {
    let snapshot;
    if (filters.productId) {
      snapshot = await db.collection(COLLECTIONS.REVIEWS)
        .where('productId', '==', filters.productId)
        .orderBy('createdAt', 'desc')
        .get();
    } else {
      snapshot = await db.collection(COLLECTIONS.REVIEWS).orderBy('createdAt', 'desc').get();
    }

    return listFromCollection(snapshot);
  },

  async create(payload) {
    const id = payload.id || randomId('review');
    const review = {
      id,
      productId: payload.productId,
      userId: payload.userId,
      orderId: payload.orderId || null,
      rating: payload.rating,
      title: `${payload.title || ''}`.trim(),
      body: `${payload.body || ''}`.trim(),
      tags: payload.tags || [],
      helpfulCount: 0,
      hidden: false,
      reportCount: 0,
      createdAt: now(),
      updatedAt: now(),
    };

    if (!review.userId) {
      throw new Error('User id required');
    }
    if (!review.productId || !Number.isFinite(review.rating) || review.rating < 1 || review.rating > 5) {
      throw new Error('Invalid review payload');
    }

    await db.collection(COLLECTIONS.REVIEWS).doc(id).set(review);

    const product = await productsRepo.getById(review.productId);
    if (product) {
      const reviews = await reviewsRepo.list({ productId: review.productId });
      const visible = reviews.filter((item) => !item.hidden);
      const avg = visible.length
        ? visible.reduce((sum, item) => sum + Number(item.rating), 0) / visible.length
        : review.rating;
      await productsRepo.patch(review.productId, {
        ratingAverage: Number(avg.toFixed(2)),
        reviewCount: visible.length,
      });
    }

    return review;
  },

  async report(id, reason) {
    const snap = await db.collection(COLLECTIONS.REVIEWS).doc(id).get();
    if (!snap.exists) {
      throw new Error('Review not found');
    }
    const reportCount = Number(snap.data().reportCount || 0) + 1;
    await db.collection(COLLECTIONS.REVIEWS).doc(id).update({
      reportCount,
      hidden: reportCount >= 3,
      updatedAt: now(),
      lastReportReason: reason || 'unspecified',
    });
    return reviewsRepo.getById(id);
  },

  async helpful(id) {
    const snap = await db.collection(COLLECTIONS.REVIEWS).doc(id).get();
    if (!snap.exists) {
      throw new Error('Review not found');
    }
    const helpfulCount = Number(snap.data().helpfulCount || 0) + 1;
    await db.collection(COLLECTIONS.REVIEWS).doc(id).update({
      helpfulCount,
      updatedAt: now(),
    });
    return reviewsRepo.getById(id);
  },

  async getById(id) {
    const snap = await db.collection(COLLECTIONS.REVIEWS).doc(id).get();
    if (!snap.exists) {
      return null;
    }
    return toDocData(snap);
  },
};

const couponsRepo = {
  async getAllActive() {
    const snapshot = await db.collection(COLLECTIONS.COUPONS).where('active', '==', true).get();
    return listFromCollection(snapshot);
  },

  async getByCode(code) {
    const norm = `${code || ''}`.trim().toUpperCase();
    const snapshot = await db.collection(COLLECTIONS.COUPONS).where('code', '==', norm).limit(1).get();
    if (snapshot.empty) {
      return null;
    }
    return toDocData(snapshot.docs[0]);
  },

  async upsertMany(coupons) {
    const batch = db.batch();
    coupons.forEach((coupon) => {
      const id = coupon.id || randomId('coupon');
      batch.set(db.collection(COLLECTIONS.COUPONS).doc(id), {
        id,
        code: `${coupon.code}`.trim().toUpperCase(),
        type: coupon.type || 'percentage',
        value: coupon.value,
        department: coupon.department || null,
        minimumSubtotalCents: coupon.minimumSubtotalCents || 0,
        expiresAt: coupon.expiresAt || null,
        active: Boolean(coupon.active ?? true),
        createdAt: now(),
      });
    });
    await batch.commit();
  },
};

const ordersRepo = {
  async create(payload, tx = null) {
    const id = payload.id || randomId('order');
    const order = {
      id,
      userId: payload.userId,
      items: payload.items,
      totals: payload.totals,
      couponSnapshot: payload.couponSnapshot || null,
      addressSnapshot: payload.addressSnapshot || null,
      paymentMask: payload.paymentMask || null,
      status: payload.status || 'processing',
      statusTimeline: payload.statusTimeline || [
        {
          status: payload.status || 'processing',
          changedAt: now(),
          actor: payload.userId,
        },
      ],
      createdAt: now(),
      updatedAt: now(),
    };
    if (tx) {
      tx.set(db.collection(COLLECTIONS.ORDERS).doc(id), order);
    } else {
      await db.collection(COLLECTIONS.ORDERS).doc(id).set(order);
    }
    return order;
  },

  async listByUser(userId) {
    const snapshot = await db
      .collection(COLLECTIONS.ORDERS)
      .where('userId', '==', userId)
      .orderBy('createdAt', 'desc')
      .get();
    return listFromCollection(snapshot);
  },

  async getById(userId, orderId) {
    const snap = await db.collection(COLLECTIONS.ORDERS).doc(orderId).get();
    if (!snap.exists) {
      return null;
    }
    const order = toDocData(snap);
    if (order.userId !== userId) {
      return null;
    }
    return order;
  },

  async setStatus(orderId, status, actor, note = '') {
    const snap = await db.collection(COLLECTIONS.ORDERS).doc(orderId).get();
    if (!snap.exists) {
      throw new Error('Order not found');
    }

    const order = snap.data();
    const nextTimeline = [...(order.statusTimeline || []), {
      status,
      changedAt: now(),
      actor: actor || 'system',
      note,
    }];

    await db.collection(COLLECTIONS.ORDERS).doc(orderId).update({
      status,
      statusTimeline: nextTimeline,
      updatedAt: now(),
    });
    return ordersRepo.getById(order.userId || actor, orderId);
  },
};

const notificationsRepo = {
  async listForUser(userId) {
    const snap = await db
      .collection(COLLECTIONS.NOTIFICATIONS)
      .where('userId', '==', userId)
      .orderBy('createdAt', 'desc')
      .get();
    return listFromCollection(snap);
  },

  async create(userId, payload) {
    const id = payload.id || randomId('notif');
    const notification = {
      id,
      userId,
      type: payload.type,
      title: payload.title,
      body: payload.body,
      read: false,
      createdAt: now(),
    };
    await db.collection(COLLECTIONS.NOTIFICATIONS).doc(id).set(notification);
    return notification;
  },

  async markRead(userId, id) {
    const notification = await db.collection(COLLECTIONS.NOTIFICATIONS).doc(id).get();
    if (!notification.exists || notification.data().userId !== userId) {
      throw new Error('Notification not found');
    }
    await db.collection(COLLECTIONS.NOTIFICATIONS).doc(id).update({
      read: true,
      updatedAt: now(),
    });
    const snap = await db.collection(COLLECTIONS.NOTIFICATIONS).doc(id).get();
    return toDocData(snap);
  },

  async markAllRead(userId) {
    const snapshot = await db
      .collection(COLLECTIONS.NOTIFICATIONS)
      .where('userId', '==', userId)
      .where('read', '==', false)
      .get();
    const batch = db.batch();
    snapshot.docs.forEach((doc) => {
      batch.update(doc.ref, { read: true, updatedAt: now() });
    });
    await batch.commit();
    return notificationsRepo.listForUser(userId);
  },
};

const moderationRepo = {
  async create(payload) {
    const id = payload.id || randomId('moderation');
    const event = {
      id,
      actorId: payload.actorId || 'system',
      targetType: payload.targetType,
      targetId: payload.targetId,
      action: payload.action,
      reason: payload.reason || null,
      status: payload.status || 'open',
      createdAt: now(),
      updatedAt: now(),
    };
    await db.collection(COLLECTIONS.MODERATION).doc(id).set(event);
    return event;
  },

  async listOpen() {
    const snapshot = await db
      .collection(COLLECTIONS.MODERATION)
      .where('status', '==', 'open')
      .orderBy('createdAt', 'desc')
      .get();
    return listFromCollection(snapshot);
  },

  async setAction(id, action, actorId) {
    const docRef = db.collection(COLLECTIONS.MODERATION).doc(id);
    const snap = await docRef.get();
    if (!snap.exists) {
      throw new Error('Moderation event not found');
    }
    await docRef.update({
      actionTaken: action,
      reviewedBy: actorId,
      status: 'resolved',
      resolvedAt: now(),
      updatedAt: now(),
    });
    return toDocData((await docRef.get()));
  },
};

const sellerProfilesRepo = {
  async create(payload) {
    const id = payload.id || randomId('seller');
    const profile = {
      id,
      userId: payload.userId,
      displayName: payload.displayName,
      supportEmail: payload.supportEmail || null,
      ratingAverage: 0,
      active: true,
      createdAt: now(),
      updatedAt: now(),
    };
    await db.collection(COLLECTIONS.SELLER_PROFILES).doc(id).set(profile);
    return profile;
  },

  async getByUserId(userId) {
    const snapshot = await db.collection(COLLECTIONS.SELLER_PROFILES).where('userId', '==', userId).limit(1).get();
    if (snapshot.empty) {
      return null;
    }
    return toDocData(snapshot.docs[0]);
  },

  async getAll() {
    const snapshot = await db.collection(COLLECTIONS.SELLER_PROFILES).get();
    return listFromCollection(snapshot);
  },
};

const metricsRepo = {
  async inc(key, amount = 1) {
    const id = 'global';
    const ref = db.collection(COLLECTIONS.METRICS).doc(id);
    const doc = await ref.get();
    const payload = doc.exists ? doc.data() : { requestCount: 0, errorCount: 0, checkoutAttempts: 0, ordersCreated: 0, latenciesMs: [] };
    const updated = {
      ...payload,
      [key]: Number(payload[key] || 0) + amount,
      updatedAt: now(),
    };
    await ref.set(updated, { merge: true });
    return updated;
  },

  async recordLatency(ms) {
    const id = 'global';
    const ref = db.collection(COLLECTIONS.METRICS).doc(id);
    const doc = await ref.get();
    const payload = doc.exists ? doc.data() : { requestCount: 0, errorCount: 0, checkoutAttempts: 0, ordersCreated: 0, latenciesMs: [] };
    const latencies = payload.latenciesMs || [];
    latencies.push(ms);
    if (latencies.length > 100) {
      latencies.shift();
    }
    payload.latenciesMs = latencies;
    payload.updatedAt = now();
    await ref.set(payload, { merge: true });
    return payload;
  },

  async getSnapshot() {
    const snapshot = await db.collection(COLLECTIONS.METRICS).doc('global').get();
    if (!snapshot.exists) {
      return {
        requestCount: 0,
        errorCount: 0,
        checkoutAttempts: 0,
        ordersCreated: 0,
        latenciesMs: [],
      };
    }

    const data = snapshot.data();
    const latencies = data.latenciesMs || [];
    if (!Array.isArray(latencies) || latencies.length === 0) {
      return { ...data, latenciesMs: [] };
    }

    const sum = latencies.reduce((acc, value) => acc + Number(value || 0), 0);
    const latencySummary = {
      min: Math.min(...latencies),
      max: Math.max(...latencies),
      avg: Math.round(sum / latencies.length),
      samples: latencies.length,
    };
    return {
      ...data,
      latencySummary,
    };
  },
};

async function withTransaction(handler) {
  return db.runTransaction(handler);
}

function ready() {
  return db
    .collection(COLLECTIONS.USERS)
    .limit(1)
    .get()
    .then(() => true)
    .catch(() => false);
}

module.exports = {
  db,
  COLLECTIONS,
  cents,
  moneyFromCents,
  deterministicId,
  randomId,
  sanitizeObject,
  withTimestamps,
  toDocData,
  listFromCollection,
  clearCollections,
  usersRepo,
  sessionsRepo,
  productsRepo,
  inventoryRepo,
  cartsRepo,
  wishlistsRepo,
  addressesRepo,
  reviewsRepo,
  couponsRepo,
  ordersRepo,
  notificationsRepo,
  moderationRepo,
  sellerProfilesRepo,
  metricsRepo,
  withTransaction,
  ready,
};
