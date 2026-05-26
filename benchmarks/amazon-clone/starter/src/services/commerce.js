const { centsFromMoney, normalizeMoneyFromCents } = require('../utils/currency');
const { normalizeMoneyFromCents: normalizeMoney, centsFromMoney: parseMoney } = require('../utils/currency');

function toNumber(value, fallback = 0) {
  const numeric = Number.parseFloat(`${value}`);
  return Number.isFinite(numeric) ? numeric : fallback;
}

function toBoolean(value) {
  if (value === undefined || value === null) return false;
  if (typeof value === 'boolean') return value;
  const normalized = `${value}`.trim().toLowerCase();
  return normalized === 'true' || normalized === '1' || normalized === 'yes' || normalized === 'on';
}

function clampInt(value, min, max) {
  const parsed = Number.parseInt(`${value}`, 10);
  if (!Number.isFinite(parsed)) {
    return min;
  }
  return Math.min(Math.max(parsed, min), max);
}

function toCents(value) {
  if (typeof value === 'number' && Number.isFinite(value)) {
    return Math.max(0, Math.round(value * 100));
  }
  if (typeof value === 'string') {
    const parsed = centsFromMoney(value);
    return Number.isFinite(parsed) ? parsed : NaN;
  }
  return NaN;
}

function formatMoneyFromCents(cents) {
  return normalizeMoney(cents || 0);
}

function ensureId(value) {
  if (!value || typeof value !== 'string') {
    return null;
  }
  const trimmed = value.trim();
  if (!trimmed.length) {
    return null;
  }
  return trimmed;
}

function safeString(value, fallback = '') {
  if (typeof value !== 'string') return fallback;
  const trimmed = value.trim();
  return trimmed.length ? trimmed : fallback;
}

function productToPublic(doc) {
  if (!doc) return null;
  const priceCents = Number.isFinite(doc.priceCents) ? doc.priceCents : toCents(doc.price);
  const stockOnHand = Number.isFinite(doc.stockOnHand) ? doc.stockOnHand : Number.isFinite(doc.stock) ? doc.stock : 0;
  const ratingAverage = Number.isFinite(doc.ratingAverage) ? doc.ratingAverage : 0;
  const reviewCount = Number.isFinite(doc.reviewCount) ? doc.reviewCount : 0;

  return {
    id: doc.id,
    title: doc.title,
    brand: doc.brand || doc.vendor || 'Amazon',
    department: doc.department || doc.category || 'General',
    category: doc.department || doc.category || 'General',
    description: doc.description || '',
    priceCents,
    price: toNumber(priceCents / 100, 0),
    listPriceCents: Number.isFinite(doc.listPriceCents) ? doc.listPriceCents : Math.round(priceCents * 1.08),
    listPrice: Number.isFinite(doc.listPriceCents) ? doc.listPriceCents / 100 : Number((priceCents / 100 * 1.08).toFixed(2)),
    image: doc.image || (Array.isArray(doc.imageUrls) ? doc.imageUrls[0] : doc.imageUrl || ''),
    imageUrls: Array.isArray(doc.imageUrls) ? doc.imageUrls : doc.image ? [doc.image] : [],
    ratingAverage: Number(ratingAverage.toFixed(2)),
    reviewCount,
    tags: Array.isArray(doc.tags) ? doc.tags : [],
    stockOnHand,
    stock: stockOnHand,
    active: toBoolean(doc.active),
    suspended: toBoolean(doc.suspended),
    sellerId: doc.sellerId || null,
    deliveryDays: Number.isFinite(doc.deliveryDays) ? doc.deliveryDays : 3,
    deliveryPromise: `${toNumber(doc.deliveryDays, 3)} day delivery`,
    createdAt: doc.createdAt || null,
    updatedAt: doc.updatedAt || null,
  };
}

function orderItemToDisplay(item) {
  return {
    productId: item.productId,
    title: item.title || item.name || '',
    priceCents: Number.isFinite(item.priceCents) ? item.priceCents : toCents(item.price || item.unitPrice),
    price: toNumber(Number.isFinite(item.priceCents) ? item.priceCents : toCents(item.price || item.unitPrice) / 100, 0),
    quantity: Number.isFinite(item.quantity) ? Math.max(1, Math.floor(item.quantity)) : 1,
    totalCents: Number.isFinite(item.totalCents) ? item.totalCents : Number.isFinite(item.priceCents) ? item.priceCents * Math.max(1, Math.floor(item.quantity)) : 0,
    total: 0,
  };
}

function normalizeReviewPayload(payload = {}) {
  const rating = clampInt(payload.rating, 1, 5);
  return {
    rating: Math.max(1, Math.min(5, rating || 5)),
    title: safeString(payload.title),
    body: safeString(payload.body),
    tags: Array.isArray(payload.tags) ? payload.tags : [],
    userId: ensureId(payload.userId) || null,
    orderId: ensureId(payload.orderId) || null,
    productId: ensureId(payload.productId),
  };
}

function parseAddressPayload(payload = {}) {
  const street = safeString(payload.street || payload.addressLine || payload.address);
  const recipient = safeString(payload.recipient, 'Recipient');
  const city = safeString(payload.city);
  const region = safeString(payload.region || payload.state, 'N/A');
  const postalCode = safeString(payload.postalCode || payload.zip || payload.zipCode);
  const country = safeString(payload.country, 'US');
  const phone = safeString(payload.phone);

  if (!street || !city || !region || !postalCode) {
    throw new Error('invalid-address');
  }

  return { recipient, street, unit: safeString(payload.unit), city, region, postalCode, country, phone };
}

function computeCheckoutTotals(items = [], coupon = null, opts = {}) {
  const subtotalCents = items.reduce((sum, item) => {
    const linePrice = Number.isFinite(item.priceCents) ? item.priceCents : toCents(item.price);
    const qty = Number.isFinite(item.quantity) ? Math.max(0, Math.floor(item.quantity)) : 0;
    return sum + (linePrice * qty);
  }, 0);

  const shippingCents = subtotalCents > 0 && subtotalCents < 6000 ? 899 : 0;
  const taxRate = toNumber(opts.taxRate, 0.0825);
  const discount = toNumber(opts.discountCents, 0);

  const taxable = Math.max(0, subtotalCents - discount);
  const taxCents = Math.round(taxable * taxRate);
  const grandTotalCents = Math.max(0, subtotalCents - discount + shippingCents + taxCents);

  return {
    subtotalCents,
    discountCents: discount,
    shippingCents,
    taxCents,
    grandTotalCents,
    totalCents: grandTotalCents,
  };
}

function applyCouponToTotals(subtotalCents, coupon = null) {
  if (!coupon || !coupon.active) {
    return { couponCode: null, discountCents: 0, couponSnapshot: null };
  }

  const code = safeString(coupon.code, '').toUpperCase();
  if (!code) {
    return { couponCode: null, discountCents: 0, couponSnapshot: null };
  }

  const minSubtotalCents = Number.isFinite(coupon.minimumSubtotalCents) ? coupon.minimumSubtotalCents : 0;
  if (subtotalCents < minSubtotalCents) {
    throw new Error('coupon-minimum-not-met');
  }

  let discountCents = 0;
  if (coupon.type === 'percent') {
    discountCents = Math.round((toNumber(coupon.value, 0) / 100) * subtotalCents);
  } else if (coupon.type === 'fixed') {
    discountCents = Math.round(toNumber(coupon.value, 0));
  }

  const maxDiscount = Number.isFinite(coupon.maxDiscountCents) && coupon.maxDiscountCents > 0
    ? coupon.maxDiscountCents
    : discountCents;
  const finalDiscount = Math.max(0, Math.min(discountCents, maxDiscount, subtotalCents));

  return {
    couponCode: code,
    couponSnapshot: {
      code,
      type: coupon.type,
      value: coupon.value,
      department: coupon.department || null,
      discountCents: finalDiscount,
    },
    discountCents: finalDiscount,
  };
}

function maskCardNumber(cardNumber) {
  const digits = `${cardNumber || ''}`.replace(/\D/g, '');
  if (digits.length < 4) {
    return '****';
  }
  return `****${digits.slice(-4)}`;
}

function buildOrderPayload({userId, sessionId, address, couponSnapshot, totals, payload = {}, items = []}) {
  if (!address || !address.street) {
    throw new Error('invalid-address');
  }
  if (!Array.isArray(items) || !items.length) {
    throw new Error('empty-cart');
  }

  const now = new Date();
  const estimatedDate = new Date(now.getTime() + 4 * 24 * 60 * 60 * 1000);

  return {
    userId,
    sessionId,
    items: items.map(orderItemToDisplay),
    status: 'processing',
    totals: {
      subtotalCents: totals.subtotalCents,
      discountCents: totals.discountCents,
      shippingCents: totals.shippingCents,
      taxCents: totals.taxCents,
      grandTotalCents: totals.grandTotalCents,
    },
    couponSnapshot: couponSnapshot || null,
    addressSnapshot: address,
    paymentMask: maskCardNumber(payload.cardNumber || payload.payment?.cardNumber || ''),
    totalsDisplay: {
      subtotal: formatMoneyFromCents(totals.subtotalCents),
      discount: formatMoneyFromCents(totals.discountCents),
      shipping: formatMoneyFromCents(totals.shippingCents),
      tax: formatMoneyFromCents(totals.taxCents),
      grandTotal: formatMoneyFromCents(totals.grandTotalCents),
    },
    createdAt: now.toISOString(),
    createdAtMs: Date.now(),
    email: safeString(payload.email || payload.contactEmail),
    fullName: safeString(payload.fullName || payload.name || payload.contactName),
  };
}

function parseCheckoutPayload(raw = {}) {
  if (raw.shippingAddress && raw.payment) {
    return {
      email: safeString(raw.email, ''),
      fullName: safeString(raw.shippingAddress.name, ''),
      address: {
        recipient: safeString(raw.shippingAddress.name),
        street: safeString(raw.shippingAddress.line1 || raw.shippingAddress.street),
        unit: safeString(raw.shippingAddress.line2 || ''),
        city: safeString(raw.shippingAddress.city),
        region: safeString(raw.shippingAddress.region || raw.shippingAddress.state),
        postalCode: safeString(raw.shippingAddress.postalCode || raw.shippingAddress.zip),
        country: safeString(raw.shippingAddress.country, 'US'),
        phone: safeString(raw.shippingAddress.phone),
      },
      cardNumber: safeString(raw.payment.cardNumber),
      expiryDate: safeString(raw.payment.expiry || raw.payment.expiryDate),
      cvv: safeString(raw.payment.cvv),
    };
  }

  return {
    email: safeString(raw.email, ''),
    fullName: safeString(raw.fullName || raw.name, ''),
    address: {
      recipient: safeString(raw.fullName || raw.name, ''),
      street: safeString(raw.address),
      unit: safeString(raw.unit),
      city: safeString(raw.city),
      region: safeString(raw.region || raw.state),
      postalCode: safeString(raw.postalCode || raw.zip),
      country: safeString(raw.country, 'US'),
      phone: safeString(raw.phone),
    },
    cardNumber: safeString(raw.cardNumber),
    expiryDate: safeString(raw.expiryDate || raw.expiry || raw.cardExpiry),
    cvv: safeString(raw.cvv),
  };
}

function isCardValid(payload = {}) {
  if (!payload.cardNumber || !/^\d{13,19}$/.test(`${payload.cardNumber}`.replace(/\D/g, ''))) {
    return false;
  }
  if (!/^\d{3,4}$/.test(`${payload.cvv}`.trim ? payload.cvv : `${payload.cvv}`)) {
    return false;
  }
  if (!/^\d{2}\/\d{2}$/.test(`${payload.expiryDate || payload.expiry || ''}`.trim())) {
    return false;
  }
  return true;
}

module.exports = {
  clampInt,
  toBoolean,
  toNumber,
  toCents,
  formatMoneyFromCents,
  ensureId,
  productToPublic,
  orderItemToDisplay,
  normalizeReviewPayload,
  parseAddressPayload: parseAddressPayload,
  computeCheckoutTotals,
  applyCouponToTotals,
  maskCardNumber,
  buildOrderPayload,
  parseCheckoutPayload,
  isCardValid,
};
