const crypto = require('crypto');

function deterministicSeed(prefix, seed) {
  return `${prefix}_${seed}`;
}

function randomId(prefix = 'id') {
  const random = `${Math.random().toString(36).slice(2, 10)}${Date.now().toString(36)}`;
  return `${prefix}_${random}`;
}

function shortHash(input) {
  return crypto.createHash('sha256').update(input).digest('hex').slice(0, 12);
}

module.exports = {
  deterministicSeed,
  randomId,
  shortHash,
};
