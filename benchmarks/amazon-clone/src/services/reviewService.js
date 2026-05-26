const { reviewsRepo } = require('../storage/firestoreStore');

async function listReviews(filters = {}) {
  const reviews = await reviewsRepo.list(filters);
  return reviews.map((review) => ({
    ...review,
    rating: Number(review.rating || 0),
  }));
}

async function createReview(user, payload = {}) {
  const rating = Number(payload.rating);
  if (!Number.isFinite(rating) || rating < 1 || rating > 5) {
    throw new Error('Rating must be 1-5');
  }
  if (!payload.productId) {
    throw new Error('Product id required');
  }
  if (!payload.body || !`${payload.body}`.trim()) {
    throw new Error('Review body required');
  }

  const title = `${payload.title || ''}`.trim();
  return reviewsRepo.create({
    userId: user.id,
    productId: `${payload.productId}`,
    orderId: payload.orderId || null,
    rating,
    title,
    body: `${payload.body}`.trim(),
    tags: payload.tags || [],
  });
}

async function reportReview(reviewId, reason = 'reported') {
  return reviewsRepo.report(reviewId, reason);
}

async function markHelpful(reviewId) {
  return reviewsRepo.helpful(reviewId);
}

module.exports = {
  listReviews,
  createReview,
  reportReview,
  markHelpful,
};
