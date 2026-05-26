const admin = require('firebase-admin');
const { FIRESTORE_EMULATOR_HOST, FIREBASE_PROJECT_ID } = require('./constants');

function getEmulatorHost() {
  const host = FIRESTORE_EMULATOR_HOST || '';
  if (!host) return null;
  if (host.includes('://')) {
    try {
      const parsed = new URL(host);
      return `${parsed.hostname}:${parsed.port || 8080}`;
    } catch (_error) {
      return host;
    }
  }
  return host;
}

function initDb() {
  if (!admin.apps.length) {
    admin.initializeApp({
      projectId: FIREBASE_PROJECT_ID,
    });
  }

  const db = admin.firestore();
  const host = getEmulatorHost();

  if (host) {
    db.settings({
      host,
      ssl: false,
    });
  }

  return db;
}

const db = initDb();

module.exports = {
  db,
};
