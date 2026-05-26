const { couponsRepo } = require('../storage/firestoreStore');

async function getCouponByCode(code) {
  if (!code) {
    return null;
  }
  const coupon = await couponsRepo.getByCode(code);
  if (!coupon || !coupon.active) {
    return null;
  }
  if (coupon.expiresAt && new Date(coupon.expiresAt).getTime() < Date.now()) {
    return null;
  }
  return coupon;
}

async function listCoupons() {
  return couponsRepo.getAllActive();
}

async function createCoupons(coupons) {
  return couponsRepo.upsertMany(coupons);
}

module.exports = {
  getCouponByCode,
  listCoupons,
  createCoupons,
};
