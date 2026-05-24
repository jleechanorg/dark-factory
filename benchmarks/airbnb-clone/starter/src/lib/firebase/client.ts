// Sprint 2 initializes the Firebase client SDK with emulator detection.
// Reference shape:
//   import { initializeApp, getApps } from 'firebase/app';
//   import { getAuth, connectAuthEmulator } from 'firebase/auth';
//   import { getFirestore, connectFirestoreEmulator } from 'firebase/firestore';
//   import { getStorage, connectStorageEmulator } from 'firebase/storage';
//
//   const firebaseConfig = { projectId: process.env.NEXT_PUBLIC_FIREBASE_PROJECT_ID, ... };
//   const app = getApps().length ? getApps()[0] : initializeApp(firebaseConfig);
//   if (process.env.NEXT_PUBLIC_USE_EMULATORS === 'true') { ... }

export const clientApp = null;
