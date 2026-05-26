const { SESSION_COOKIE_NAME, SESSION_DURATION_MS } = require('../config/constants');

function normalizeSuccessPayload(payload) {
  if (payload === undefined || payload === null) {
    return { ok: true };
  }
  return payload;
}

function sendSuccess(res, statusCode, payload = {}) {
  const body = normalizeSuccessPayload(payload);
  res.status(statusCode).json(body);
}

function sendError(res, statusCode, code, message, details = null) {
  const payload = {
    success: false,
    error: message,
    errorCode: code,
  };

  if (details) {
    payload.errorDetails = details;
  }

  res.status(statusCode).json(payload);
}

function normalizeMoneyFromCents(valueInCents) {
  if (typeof valueInCents !== 'number' || Number.isNaN(valueInCents)) {
    return 0;
  }
  return Number((valueInCents / 100).toFixed(2));
}

function centsFromMoney(value) {
  if (typeof value === 'number') {
    return Math.round(value * 100);
  }
  const cleaned = Number.parseFloat(`${value}`.replace(/[^0-9.]/g, ''));
  if (Number.isNaN(cleaned)) {
    return NaN;
  }
  return Math.round(cleaned * 100);
}

function maskCard(cardNumber) {
  const digits = `${cardNumber || ''}`.replace(/\D/g, '');
  if (digits.length < 4) {
    return '****';
  }
  return `****${digits.slice(-4)}`;
}

function parseExpiration(value) {
  if (!value || typeof value !== 'string') {
    return null;
  }
  const trimmed = value.trim();
  if (/^\d{2}\/\d{2}$/.test(trimmed)) {
    return trimmed;
  }
  if (/^\d{4}-\d{2}$/.test(trimmed)) {
    const [yy, mm] = trimmed.split('-');
    return `${mm}/${yy.slice(-2)}`;
  }
  if (/^\d{2}-\d{2}$/.test(trimmed)) {
    return trimmed;
  }
  return null;
}

function setSessionCookie(res, token) {
  res.cookie(SESSION_COOKIE_NAME, token, {
    httpOnly: true,
    sameSite: 'lax',
    maxAge: SESSION_DURATION_MS,
    path: '/',
  });
}

function clearSessionCookie(res) {
  res.clearCookie(SESSION_COOKIE_NAME, {
    httpOnly: true,
    sameSite: 'lax',
    path: '/',
  });
}

module.exports = {
  sendSuccess,
  sendError,
  normalizeMoneyFromCents,
  centsFromMoney,
  maskCard,
  parseExpiration,
  setSessionCookie,
  clearSessionCookie,
};
