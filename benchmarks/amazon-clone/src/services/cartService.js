const {
  cartsRepo,
  couponsRepo,
  productsRepo,
  wishlistsRepo,
} = require('../storage/firestoreStore');

function computeTotals(items, coupon) {
  let subtotalCents = 0;
  items.forEach((item) => {
    subtotalCents += (item.priceAtAddCents || 0) * item.quantity;
  });

  let discountCents = 0;
  if (coupon) {
    if (coupon.type === 'fixed') {
      discountCents = Math.min(Number(coupon.value || 0), subtotalCents);
    }

    if (coupon.type === 'percentage') {
      discountCents = Math.round((subtotalCents * Number(coupon.value || 0)) / 100);
    }
  }

  if (coupon && coupon.minimumSubtotalCents && subtotalCents < coupon.minimumSubtotalCents) {
    discountCents = 0;
  }

  const shippingCents = subtotalCents > 5000 ? 0 : 499;
  const taxableCents = Math.max(0, subtotalCents - discountCents);
  const taxCents = Math.round(taxableCents * 0.08);

  return {
    subtotalCents,
    discountCents,
    shippingCents,
    taxCents,
    totalCents: taxableCents + shippingCents + taxCents,
    couponCode: coupon ? coupon.code : null,
  };
}

function roundToCents(valueInCents) {
  if (!Number.isFinite(valueInCents)) {
    return 0;
  }
  return Math.max(0, Math.round(valueInCents));
}

async function hydrateCart(userId, cart) {
  const enrichedItems = [];
  for (const item of cart.items || []) {
    const product = await productsRepo.getById(item.productId);
    if (!product) {
      continue;
    }
    const priceAtAdd = item.priceAtAddCents || product.priceCents;
    enrichedItems.push({
      ...item,
      priceAtAddCents: priceAtAdd,
      title: product.title,
      price: Number((priceAtAdd / 100).toFixed(2)),
      stock: product.stock,
      department: product.department,
      image: (product.imageUrls || [])[0] || '',
      totalCents: priceAtAdd * item.quantity,
    });
  }

  let coupon = null;
  if (cart.couponCode) {
    coupon = await couponsRepo.getByCode(cart.couponCode);
  }

  const totals = computeTotals(enrichedItems, coupon);
  return {
    ...cart,
    items: enrichedItems,
    totalsPreview: totals,
  };
}

async function getCart(userId) {
  const cart = await cartsRepo.get(userId);
  const result = await hydrateCart(userId, cart);
  return {
    id: result.id || userId,
    userId,
    items: result.items,
    saveForLater: result.savedForLater || [],
    couponCode: result.couponCode || null,
    totalsPreview: {
      subtotalCents: roundToCents(result.totalsPreview.subtotalCents || 0),
      discountCents: roundToCents(result.totalsPreview.discountCents || 0),
      shippingCents: roundToCents(result.totalsPreview.shippingCents || 0),
      taxCents: roundToCents(result.totalsPreview.taxCents || 0),
      totalCents: roundToCents(result.totalsPreview.totalCents || 0),
    },
    updatedAt: result.updatedAt || new Date().toISOString(),
  };
}

async function addItem(userId, productId, quantity) {
  const cart = await cartsRepo.addItem(userId, productId, quantity);
  return getCart(userId);
}

async function setItemQuantity(userId, productId, quantity) {
  const cart = await cartsRepo.setItemQuantity(userId, productId, Number(quantity));
  return getCart(userId);
}

async function removeItem(userId, productId) {
  await cartsRepo.removeItem(userId, productId);
  return getCart(userId);
}

async function clearCart(userId) {
  await cartsRepo.clear(userId);
  return getCart(userId);
}

async function applyCoupon(userId, code) {
  await cartsRepo.applyCoupon(userId, code);
  return getCart(userId);
}

async function saveForLater(userId, productId) {
  const cart = await cartsRepo.saveForLater(userId, productId);
  return getCart(userId);
}

async function getWishlistState(userId) {
  return {
    items: await wishlistsRepo.asProducts(userId),
    count: ((await wishlistsRepo.getByUser(userId)).productIds || []).length,
  };
}

module.exports = {
  getCart,
  addItem,
  setItemQuantity,
  removeItem,
  clearCart,
  applyCoupon,
  saveForLater,
  hydrateCart,
  computeTotals,
  getWishlistState,
};
