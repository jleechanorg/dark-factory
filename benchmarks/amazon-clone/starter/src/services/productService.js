const { productsRepo, inventoryRepo, reviewsRepo } = require('../storage/firestoreStore');

function parseListFilters(query = {}) {
  const search = `${query.search || ''}`.trim();
  const department = `${query.department || query.category || ''}`.trim();
  const maxPrice = query.maxPrice ? Number(query.maxPrice) : null;
  const minPrice = query.minPrice ? Number(query.minPrice) : null;
  const minRating = query.minRating ? Number(query.minRating) : null;
  const minReviewCount = query.minReviewCount ? Number(query.minReviewCount) : null;

  const fastDelivery = `${query.fastDelivery || ''}` === 'true';
  const stockState = query.stockState || null;
  const seller = `${query.seller || ''}`.trim() || null;
  const discountOnly = `${query.discountOnly || ''}` === 'true';
  const sort = `${query.sort || 'relevance'}`;

  return {
    search,
    department,
    maxPrice,
    minPrice,
    minRating,
    minReviewCount,
    fastDelivery,
    stockState,
    seller,
    discountOnly,
    tags: query.tags ? `${query.tags}`.split(',').map((tag) => tag.trim()).filter(Boolean) : [],
    sort,
  };
}

function normalizeProduct(product) {
  if (!product) {
    return null;
  }

  return {
    ...product,
    price: Number(product.price || 0),
    listPrice: Number(product.listPrice || 0),
    stock: Number(product.stock || 0),
    savingsPercent: Number(product.listPrice && product.price
      ? Math.max(0, Math.round(((product.listPrice - product.price) / product.listPrice) * 100))
      : 0),
  };
}

async function listProducts(query = {}) {
  const filters = parseListFilters(query);
  const items = await productsRepo.list(filters);

  const transformed = items.map(normalizeProduct);
  return {
    items: transformed,
    total: transformed.length,
  };
}

async function getProductWithContext(productId) {
  const product = await productsRepo.getById(productId);
  if (!product) {
    return null;
  }
  const reviews = await reviewsRepo.list({ productId });
  return {
    ...normalizeProduct(product),
    reviews,
    reviewCount: reviews.length,
    ratingAverage: product.ratingAverage || 0,
  };
}

async function createProduct(payload, actor) {
  if (!actor || actor.isGuest) {
    throw new Error('Auth required');
  }
  if (!['seller', 'admin'].includes(actor.role)) {
    throw new Error('Seller role required');
  }

  if (!payload.title || `${payload.title}`.trim().length < 3) {
    throw new Error('Product title required');
  }
  if (!payload.department || `${payload.department}`.trim().length < 2) {
    throw new Error('Department required');
  }
  if (!payload.priceCents && !payload.price && payload.price !== 0) {
    throw new Error('Price required');
  }

  const newProduct = await productsRepo.create({
    sellerId: actor.id,
    title: `${payload.title}`.trim(),
    brand: `${payload.brand || 'House Brand'}`.trim(),
    department: `${payload.department}`.trim(),
    description: `${payload.description || ''}`.trim(),
    priceCents: payload.priceCents != null ? payload.priceCents : payload.price,
    listPriceCents: payload.listPriceCents || payload.listPrice || payload.priceCents || payload.price,
    imageUrls: payload.imageUrls || [],
    tags: payload.tags || [],
    stockOnHand: payload.stockOnHand || 0,
    lowStockThreshold: payload.lowStockThreshold || 5,
    deliveryPromiseDays: Number(payload.deliveryPromiseDays || 3),
    active: true,
  });

  return getProductWithContext(newProduct.id);
}

async function updateProduct(productId, payload, actor) {
  const target = await productsRepo.getById(productId);
  if (!target) {
    throw new Error('Product not found');
  }
  if (!actor || actor.isGuest) {
    throw new Error('Auth required');
  }
  if (actor.role !== 'admin' && target.sellerId !== actor.id) {
    throw new Error('Permission denied');
  }

  const patch = { ...payload };
  delete patch.id;

  const updated = await productsRepo.patch(productId, patch);
  return getProductWithContext(updated.id || productId);
}

async function archiveProduct(productId, actor) {
  const target = await productsRepo.getById(productId);
  if (!target) {
    throw new Error('Product not found');
  }
  if (!actor || actor.isGuest) {
    throw new Error('Auth required');
  }
  if (actor.role !== 'admin' && target.sellerId !== actor.id) {
    throw new Error('Permission denied');
  }
  await productsRepo.archive(productId);
  return getProductWithContext(productId);
}

async function restockProduct(productId, payload, actor) {
  const target = await productsRepo.getById(productId);
  if (!target) {
    throw new Error('Product not found');
  }
  if (!actor || actor.isGuest) {
    throw new Error('Auth required');
  }
  if (actor.role !== 'admin' && target.sellerId !== actor.id) {
    throw new Error('Permission denied');
  }

  const delta = Number(payload.delta || payload.stock || 0);
  const adjusted = await inventoryRepo.restock(productId, delta);
  return { id: productId, stock: adjusted, ...target };
}

module.exports = {
  parseListFilters,
  listProducts,
  getProductWithContext,
  createProduct,
  updateProduct,
  archiveProduct,
  restockProduct,
  normalizeProduct,
};
