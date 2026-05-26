const { ordersRepo, productsRepo, cartsRepo } = require('../storage/firestoreStore');

async function listOrders(userId) {
  const records = await ordersRepo.listByUser(userId);
  return records.map((order) => ({
    id: order.id,
    status: order.status,
    totals: order.totals || {},
    createdAt: order.createdAt,
    itemCount: (order.items || []).reduce((acc, item) => acc + Number(item.quantity || 0), 0),
  }));
}

async function getOrder(userId, orderId) {
  const order = await ordersRepo.getById(userId, orderId);
  if (!order) {
    return null;
  }
  return {
    ...order,
    items: (order.items || []).map((item) => ({
      ...item,
      itemTotal: Number(((item.unitPriceCents || 0) * (item.quantity || 0) / 100).toFixed(2)),
      price: Number(((item.unitPriceCents || 0) / 100).toFixed(2)),
    })),
  };
}

async function reorder(user, orderId) {
  const source = await ordersRepo.getById(user.id, orderId);
  if (!source) {
    throw new Error('Order not found');
  }

  const cart = await cartsRepo.get(user.id);
  const nextItems = [];
  for (const item of source.items || []) {
    const product = await productsRepo.getById(item.productId);
    if (product && product.active) {
      nextItems.push({
        productId: product.id,
        quantity: Number(item.quantity || 0),
        priceAtAddCents: product.priceCents,
      });
    }
  }

  await cartsRepo.set(user.id, {
    ...cart,
    items: nextItems,
  });

  return getOrder(user.id, orderId);
}

module.exports = {
  listOrders,
  getOrder,
  reorder,
};
