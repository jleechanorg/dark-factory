const { addressesRepo, usersRepo } = require('../storage/firestoreStore');

function validateAddress(payload = {}) {
  const recipient = `${payload.recipient || ''}`.trim();
  const street = `${payload.street || ''}`.trim();
  const city = `${payload.city || ''}`.trim();
  const region = `${payload.region || ''}`.trim();
  const postalCode = `${payload.postalCode || ''}`.trim();

  if (!recipient || recipient.length < 2) {
    throw new Error('Recipient required');
  }
  if (!street || street.length < 5) {
    throw new Error('Street required');
  }
  if (!city || !region || !postalCode) {
    throw new Error('Address city/region/postal code required');
  }
}

async function listAddresses(userId) {
  return addressesRepo.listByUser(userId);
}

async function getAddress(userId, addressId) {
  const address = await addressesRepo.getById(addressId);
  if (!address || address.userId !== userId) {
    return null;
  }
  return address;
}

async function createAddress(userId, payload) {
  validateAddress(payload);
  const address = await addressesRepo.create(userId, payload);
  return address;
}

async function updateAddress(userId, addressId, payload) {
  validateAddress({ ...payload, isDefault: true });
  return addressesRepo.update(addressId, userId, payload);
}

async function deleteAddress(userId, addressId) {
  const result = await addressesRepo.destroy(addressId, userId);
  const user = await usersRepo.findById(userId);
  if (user && user.defaultAddressId === addressId) {
    const remaining = await addressesRepo.listByUser(userId);
    await usersRepo.setDefaultAddress(userId, remaining[0] ? remaining[0].id : null);
  }
  return result;
}

async function setDefaultAddress(userId, addressId) {
  return addressesRepo.makeDefault(userId, addressId);
}

module.exports = {
  validateAddress,
  listAddresses,
  getAddress,
  createAddress,
  updateAddress,
  deleteAddress,
  setDefaultAddress,
};
