const { notificationsRepo } = require('../storage/firestoreStore');

async function listNotifications(userId) {
  return notificationsRepo.listForUser(userId);
}

async function markNotificationRead(userId, notificationId) {
  return notificationsRepo.markRead(userId, notificationId);
}

async function markAllRead(userId) {
  return notificationsRepo.markAllRead(userId);
}

module.exports = {
  listNotifications,
  markNotificationRead,
  markAllRead,
};
