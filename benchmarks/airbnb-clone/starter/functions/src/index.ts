import * as functions from "firebase-functions";

export const health = functions.https.onRequest((_req, res) => {
  res.json({ ok: true });
});
