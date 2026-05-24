// Sprint 2 initializes firebase-admin with emulator detection.
// Reference shape:
//   import * as admin from 'firebase-admin';
//   if (process.env.NEXT_PUBLIC_USE_EMULATORS === 'true') {
//     process.env.FIRESTORE_EMULATOR_HOST = 'localhost:8080';
//     process.env.FIREBASE_AUTH_EMULATOR_HOST = 'localhost:9099';
//     process.env.FIREBASE_STORAGE_EMULATOR_HOST = 'localhost:9199';
//   }
//   if (!admin.apps.length) { admin.initializeApp({ projectId: process.env.NEXT_PUBLIC_FIREBASE_PROJECT_ID }); }

export const adminApp = null;
