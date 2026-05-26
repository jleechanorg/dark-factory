const {
  ordersRepo,
  cartsRepo,
  productsRepo,
  inventoryRepo,
  withTransaction,
  notificationsRepo,
  metricsRepo,
  cartsRepo: carts,
} = require('../storage/firestoreStore');
const {
  getCart,
  computeTotals,
} = require('./cartService');

function parseAddressPayload(payload = {}) {
  if (payload.addressId) {
    return {
      addressId: payload.addressId,
      recipient: payload.recipient || payload.name || payload.fullName,
      line1: payload.line1 || payload.address || payload.street,
      city: payload.city,
      region: payload.region || payload.state,
      postalCode: payload.postalCode || payload.zip,
      country: payload.country || 'USA',
    };
  }

  return {
    recipient: payload.fullName || payload.name || payload.recipient,
    line1: payload.address || payload.street || payload.line1,
    city: payload.city,
    region: payload.state || payload.region,
    postalCode: payload.zip || payload.postalCode,
    country: payload.country || 'USA',
    phone: payload.phone,
  };
}

function maskPayment(payment = {}) {
  const raw = `${payment.cardNumber || payment.card_token || ''}`.replace(/\D/g, '');
  if (raw.length < 4) {
    return '••••';
  }
  return `****${raw.slice(-4)}`;
}

function normalizePayment(payload = {}) {
  return {
    method: payload.method || 'card',
    cardType: payload.cardType || 'card',
    maskedLast4: maskPayment(payload),
    holder: `${payload.cardholder || payload.name || ''}`.trim(),
  };
}

function validateCheckoutPayload(payload = {}) {
  const email = `${payload.email || ''}`.trim();
  const recipient = `${payload.fullName || payload.recipient || payload.name || ''}`.trim();
  const address = payload.address || payload.addressId || payload.line1 || '';
  const city = `${payload.city || ''}`.trim();

  if (!email.includes('@')) {
    return 'Invalid email';
  }
  if (!recipient || recipient.length < 2) {
    return 'Recipient name required';
  }
  if (!address || `${address}`.trim().length < 6) {
    return 'Address required';
  }
  if (!city) {
    return 'City required';
  }
  return null;
}

async function calculateCheckoutPreview(userId) {
  const cart = await getCart(userId);
  const couponCode = cart.couponCode;
  const items = cart.items || [];
  const lines = [];
  for (const item of items) {
    lines.push({
      productId: item.productId,
      title: item.title,
      unitPriceCents: item.priceAtAddCents || 0,
      quantity: item.quantity,
      lineTotalCents: (item.priceAtAddCents || 0) * item.quantity,
    });
  }
  return {
    lines,
    totals: cart.totalsPreview,
    couponCode,
  };
}

async function checkout(user, payload = {}) {
  const invalid = validateCheckoutPayload(payload);
  if (invalid) {
    throw new Error(invalid);
  }

  await metricsRepo.inc('checkoutAttempts', 1);
  const cart = await cartsRepo.get(user.id);
  if (!cart || !(cart.items || []).length) {
    throw new Error('Cart is empty');
  }

  const cartPreview = await getCart(user.id);
  if (!cartPreview.items.length) {
    throw new Error('Cart is empty');
  }

  const lineItems = [];
  const orderLineChecks = [];

  for (const item of cartPreview.items) {
    const product = await productsRepo.getById(item.productId);
    if (!product) {
      throw new Error(`Product ${item.productId} no longer exists`);
    }
    if (!product.active) {
      throw new Error(`${product.title} is no longer for sale`);
    }
    if (item.quantity > item.stock) {
      throw new Error(`Only ${item.stock} of ${product.title} are available`);
    }
    orderLineChecks.push({
      productId: product.id,
      productTitle: product.title,
      unitPriceCents: product.priceCents,
      quantity: item.quantity,
      totalCents: product.priceCents * item.quantity,
    });
  }

  const coupon = cart.couponCode ? await require('./couponService').getCouponByCode(cart.couponCode) : null;
  const totals = computeTotals(
    (cartPreview.items || []).map((item) => ({
      priceAtAddCents: item.priceAtAddCents,
      quantity: item.quantity,
    })),
    coupon,
  );

  const addressSnapshot = parseAddressPayload(payload);
  const paymentSnapshot = normalizePayment(payload.payment || payload);

  const order = await withTransaction(async (tx) => {
    for (const line of orderLineChecks) {
      await inventoryRepo.commitOrder(line.productId, line.quantity, tx);
      lineItems.push({
        ...line,
      });
    }

    await tx.delete(carts.collection(COLLECTIONS.CARTS).doc(user.id));
    return ordersRepo.create({
      userId: user.id,
      items: lineItems,
      totals,
      couponSnapshot: cart.couponCode ? { code: cart.couponCode } : null,
      addressSnapshot,
      paymentMask: paymentSnapshot.maskedLast4,
      status: 'created',
      statusTimeline: [{
        status: 'created',
        changedAt: new Date().toISOString(),
        actor: user.id,
      }],
    }, tx);
  });

  await notificationsRepo.create(user.id, {
    type: 'order_created',
    title: 'Order created',
    body: `Your order ${order.id} has been created and is being processed.`,
  });

  await metricsRepo.inc('ordersCreated', 1);
  return {
    ...order,
    paymentMask: paymentSnapshot.maskedLast4,
  };
}

function finalizeOrderPayload(raw, order) {
  return {
    id: order.id,
    userId: order.userId,
    status: order.status,
    items: order.items,
    totals: order.totals,
    paymentMask: order.paymentMask,
    createdAt: order.createdAt || new Date().toISOString(),
  };
}

module.exports = {
  calculateCheckoutPreview,
  checkout,
  parseAddressPayload,
  normalizePayment,
  finalizeOrderPayload,
};
