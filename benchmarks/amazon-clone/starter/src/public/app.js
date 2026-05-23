// --- Amazon Clone Frontend Controller ---

// Global App State
const state = {
  products: [],
  filteredProducts: [],
  cart: { items: [] },
  wishlist: [],
  orders: [],
  user: null,
  activeView: 'products-view',
  searchQuery: '',
  selectedCategory: 'All Categories',
  detailProductId: null
};

// Config
const API_URL = '/api';
let searchDebounceTimeout = null;

// Document Ready
document.addEventListener('DOMContentLoaded', () => {
  initApp();
});

// --- App Initialization ---
async function initApp() {
  setupEventListeners();
  
  // 1. Initial State Sync
  await checkAuthStatus();
  await loadProducts();
  await loadCart();
  
  if (state.user) {
    await loadWishlist();
    await loadOrders();
  } else {
    // Guest fallback using localStorage
    loadGuestCartLocal();
    loadGuestWishlistLocal();
  }

  // 2. Initial Render
  renderProductsList();
  updateHeaderBadges();
  
  // Enable Back-to-top button
  document.getElementById('back-to-top-btn').addEventListener('click', () => {
    window.scrollTo({ top: 0, behavior: 'smooth' });
  });
}

// --- Event Listeners Setup ---
function setupEventListeners() {
  // Navigation Links
  document.getElementById('brand-logo').addEventListener('click', (e) => {
    e.preventDefault();
    switchView('products-view');
  });

  document.getElementById('auth-nav-btn').addEventListener('click', () => {
    openDrawer('auth-drawer');
  });

  document.getElementById('orders-nav-btn').addEventListener('click', () => {
    if (!state.user) {
      showToast('Please sign in to view your orders', 'error');
      openDrawer('auth-drawer');
    } else {
      switchView('orders-view');
    }
  });

  document.getElementById('wishlist-nav-btn').addEventListener('click', () => {
    if (!state.user) {
      showToast('Please sign in to view your wishlist', 'error');
      openDrawer('auth-drawer');
    } else {
      switchView('wishlist-view');
    }
  });

  document.getElementById('admin-toggle-btn').addEventListener('click', () => {
    switchView('admin-view');
  });

  document.getElementById('cart-nav-btn').addEventListener('click', () => {
    openDrawer('cart-drawer');
  });

  // Modal overlays clicks to close
  document.getElementById('cart-drawer-overlay').addEventListener('click', () => closeDrawer('cart-drawer'));
  document.getElementById('close-cart-btn').addEventListener('click', () => closeDrawer('cart-drawer'));
  document.getElementById('auth-drawer-overlay').addEventListener('click', () => closeDrawer('auth-drawer'));
  document.getElementById('close-auth-btn').addEventListener('click', () => closeDrawer('auth-drawer'));

  // Search & Filters (Debounced)
  const searchInput = document.getElementById('search-input');
  const clearSearchBtn = document.getElementById('clear-search-btn');
  const categorySelect = document.getElementById('category-select');

  searchInput.addEventListener('input', (e) => {
    state.searchQuery = e.target.value;
    clearSearchBtn.style.display = state.searchQuery ? 'block' : 'none';
    
    // 300ms Debounce
    clearTimeout(searchDebounceTimeout);
    searchDebounceTimeout = setTimeout(() => {
      filterProducts();
    }, 300);
  });

  clearSearchBtn.addEventListener('click', () => {
    searchInput.value = '';
    state.searchQuery = '';
    clearSearchBtn.style.display = 'none';
    filterProducts();
    searchInput.focus();
  });

  categorySelect.addEventListener('change', (e) => {
    state.selectedCategory = e.target.value;
    filterProducts();
  });

  document.getElementById('reset-filters-btn').addEventListener('click', () => {
    searchInput.value = '';
    state.searchQuery = '';
    clearSearchBtn.style.display = 'none';
    categorySelect.value = 'All Categories';
    state.selectedCategory = 'All Categories';
    filterProducts();
  });

  // Auth Tabs controls
  document.getElementById('tab-signin').addEventListener('click', () => toggleAuthTabs('signin'));
  document.getElementById('tab-signup').addEventListener('click', () => toggleAuthTabs('signup'));

  // Auth Forms Submissions
  document.getElementById('signin-form').addEventListener('submit', handleSignIn);
  document.getElementById('signup-form').addEventListener('submit', handleSignUp);
  document.getElementById('signout-btn').addEventListener('click', handleSignOut);

  // Detail Reviews Submission
  document.getElementById('add-review-form').addEventListener('submit', handleReviewSubmit);

  // Cart Adjustments inside drawers
  document.getElementById('proceed-checkout-btn').addEventListener('click', () => {
    closeDrawer('cart-drawer');
    switchView('checkout-view');
  });

  document.getElementById('clear-cart-btn').addEventListener('click', handleClearCart);

  // Checkout Validation Blurs & Input Masks
  setupCheckoutValidation();

  // Navigation redirect helpers
  document.querySelectorAll('.navigate-home-btn').forEach(btn => {
    btn.addEventListener('click', () => {
      switchView('products-view');
    });
  });

  document.getElementById('back-to-products-btn').addEventListener('click', () => {
    switchView('products-view');
  });
}

// --- Data Fetching & Sync ---

async function checkAuthStatus() {
  try {
    const res = await fetch(`${API_URL}/auth/me`);
    const data = await res.json();
    if (data.user) {
      state.user = data.user;
      renderAuthDrawer();
    }
  } catch (error) {
    // Quietly ignore
  }
}

async function loadProducts() {
  try {
    const res = await fetch(`${API_URL}/products`);
    const data = await res.json();
    state.products = data;
    state.filteredProducts = data;
  } catch (error) {
    showToast('Failed to load products list', 'error');
  }
}

async function loadCart() {
  try {
    const res = await fetch(`${API_URL}/cart`);
    const data = await res.json();
    state.cart = data;
  } catch (error) {
    // Quietly ignore
  }
}

async function loadWishlist() {
  if (!state.user) return;
  try {
    const res = await fetch(`${API_URL}/auth/wishlist`);
    const data = await res.json();
    state.wishlist = data;
  } catch (error) {
    // Quietly ignore
  }
}

async function loadOrders() {
  if (!state.user) return;
  try {
    const res = await fetch(`${API_URL}/orders`);
    const data = await res.json();
    state.orders = data;
  } catch (error) {
    // Quietly ignore
  }
}

// --- LocalStorage State Sync (Guest fallback) ---
function loadGuestCartLocal() {
  const localCart = localStorage.getItem('guest_cart');
  if (localCart) {
    try {
      state.cart = JSON.parse(localCart);
    } catch (e) {
      state.cart = { items: [] };
    }
  } else {
    state.cart = { items: [] };
  }
}

function saveGuestCartLocal() {
  localStorage.setItem('guest_cart', JSON.stringify(state.cart));
}

function loadGuestWishlistLocal() {
  const localWish = localStorage.getItem('guest_wishlist');
  if (localWish) {
    try {
      state.wishlist = JSON.parse(localWish);
    } catch (e) {
      state.wishlist = [];
    }
  } else {
    state.wishlist = [];
  }
}

function saveGuestWishlistLocal() {
  localStorage.setItem('guest_wishlist', JSON.stringify(state.wishlist));
}

// --- View Router Controller ---
function switchView(viewId) {
  state.activeView = viewId;
  
  // Hide all views, display active
  document.querySelectorAll('.view-section').forEach(view => {
    view.classList.remove('active');
  });
  
  const activeViewEl = document.getElementById(viewId);
  if (activeViewEl) {
    activeViewEl.classList.add('active');
  }

  // Auto scroll to top
  window.scrollTo({ top: 0 });

  // View-specific trigger updates
  if (viewId === 'products-view') {
    renderProductsList();
  } else if (viewId === 'wishlist-view') {
    renderWishlist();
  } else if (viewId === 'orders-view') {
    renderOrders();
  } else if (viewId === 'admin-view') {
    renderAdminInventory();
  } else if (viewId === 'checkout-view') {
    renderCheckoutSummary();
  }
}

function openDrawer(drawerId) {
  document.getElementById(`${drawerId}-overlay`).classList.add('active');
  document.getElementById(drawerId).classList.add('active');
  document.getElementById(drawerId).setAttribute('aria-hidden', 'false');
  
  if (drawerId === 'cart-drawer') {
    renderCart();
  }
}

function closeDrawer(drawerId) {
  document.getElementById(`${drawerId}-overlay`).classList.remove('active');
  document.getElementById(drawerId).classList.remove('active');
  document.getElementById(drawerId).setAttribute('aria-hidden', 'true');
}

// --- Star Ratings Visual Helper ---
function getStarsHTML(rating) {
  let stars = '';
  const rounded = Math.round(rating * 2) / 2; // nearest 0.5
  for (let i = 1; i <= 5; i++) {
    if (i <= rounded) {
      stars += '&#9733;'; // solid star
    } else if (i - 0.5 === rounded) {
      stars += '&#9734;'; // half star fallback, or we can use styling (solid here)
    } else {
      stars += '&#9734;'; // empty star
    }
  }
  return stars;
}

// --- Render 1: Product Listing ---
function renderProductsList() {
  const grid = document.getElementById('products-grid');
  const counter = document.getElementById('results-counter');
  const noProductsMsg = document.getElementById('no-products-message');
  
  grid.innerHTML = '';
  
  if (state.filteredProducts.length === 0) {
    noProductsMsg.style.display = 'block';
    counter.textContent = 'Showing 0 of ' + state.products.length + ' products';
    return;
  }
  
  noProductsMsg.style.display = 'none';
  counter.textContent = `Showing ${state.filteredProducts.length} of ${state.products.length} products`;

  state.filteredProducts.forEach(product => {
    const isSaved = state.wishlist.some(w => w.id === product.id);
    
    // Stock Indicator
    let stockClass = 'stock-ok';
    let stockText = 'In Stock';
    if (product.stock === 0) {
      stockClass = 'stock-out';
      stockText = 'Out of Stock';
    } else if (product.stock < 5) {
      stockClass = 'stock-warning';
      stockText = `Only ${product.stock} left in stock - order soon!`;
    }

    const card = document.createElement('div');
    card.className = 'product-card';
    card.setAttribute('role', 'article');
    card.setAttribute('aria-label', product.title);
    
    card.innerHTML = `
      <button class="wishlist-heart-btn ${isSaved ? 'active' : ''}" data-id="${product.id}" aria-label="${isSaved ? 'Remove from wishlist' : 'Add to wishlist'}">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"/>
        </svg>
      </button>
      
      <div class="product-image-wrapper" data-nav-id="${product.id}">
        <img src="${product.image}" alt="${product.title}" class="product-img" loading="lazy">
      </div>
      
      <div class="product-info" data-nav-id="${product.id}">
        <span class="product-category">${product.category}</span>
        <h3 class="product-title">${product.title}</h3>
        
        <div class="rating-container">
          <div class="stars-display" aria-hidden="true">${getStarsHTML(product.rating)}</div>
          <span class="rating-value">${product.rating}</span>
          <span class="review-count">(${product.reviews ? product.reviews.length : 0})</span>
        </div>
        
        <div class="price-container">
          <span class="price-currency">$</span>
          <span class="price-amount">${product.price.toFixed(2)}</span>
        </div>
        
        <span class="product-stock-tag ${stockClass}">${stockText}</span>
      </div>
      
      <div class="card-actions">
        <button class="btn btn-primary btn-block add-to-cart-btn" data-id="${product.id}" ${product.stock === 0 ? 'disabled' : ''}>
          ${product.stock === 0 ? 'Out of Stock' : 'Add to Cart'}
        </button>
      </div>
    `;

    // Card navigation triggers
    card.querySelectorAll('[data-nav-id]').forEach(el => {
      el.addEventListener('click', () => {
        navigateToProductDetail(product.id);
      });
    });

    // Add to Cart card action
    card.querySelector('.add-to-cart-btn').addEventListener('click', (e) => {
      e.stopPropagation();
      handleAddToCart(product.id, 1);
    });

    // Wishlist click
    card.querySelector('.wishlist-heart-btn').addEventListener('click', (e) => {
      e.stopPropagation();
      handleToggleWishlist(product.id);
    });

    grid.appendChild(card);
  });
}

// --- Search Filter Logic ---
function filterProducts() {
  let filtered = state.products;

  if (state.selectedCategory && state.selectedCategory !== 'All Categories') {
    filtered = filtered.filter(p => p.category.toLowerCase() === state.selectedCategory.toLowerCase());
  }

  if (state.searchQuery) {
    const q = state.searchQuery.toLowerCase().trim();
    filtered = filtered.filter(p => 
      p.title.toLowerCase().includes(q) || 
      p.description.toLowerCase().includes(q)
    );
  }

  state.filteredProducts = filtered;
  renderProductsList();
}

// --- Render 2: Product Detail ---
function navigateToProductDetail(productId) {
  state.detailProductId = productId;
  const product = state.products.find(p => p.id === productId);
  if (!product) return;

  const content = document.getElementById('product-detail-content');
  
  // Stock details
  let stockClass = 'stock-ok';
  let stockText = 'In Stock';
  if (product.stock === 0) {
    stockClass = 'stock-out';
    stockText = 'Currently Out of Stock';
  } else if (product.stock < 5) {
    stockClass = 'stock-warning';
    stockText = `Only ${product.stock} items left - order fast!`;
  }

  // Populate options up to stock count (cap 10)
  const maxQty = Math.min(product.stock, 10);
  let qtyOptions = '';
  if (maxQty > 0) {
    for (let i = 1; i <= maxQty; i++) {
      qtyOptions += `<option value="${i}">Qty: ${i}</option>`;
    }
  } else {
    qtyOptions = `<option value="0" disabled selected>Qty: 0</option>`;
  }

  const isSaved = state.wishlist.some(w => w.id === product.id);

  content.innerHTML = `
    <div class="detail-gallery">
      <img src="${product.image}" alt="${product.title}" class="detail-img">
    </div>
    <div class="detail-meta">
      <span class="detail-category">${product.category}</span>
      <h1 class="detail-title">${product.title}</h1>
      
      <div class="rating-container detail-rating">
        <div class="stars-display" aria-hidden="true">${getStarsHTML(product.rating)}</div>
        <span class="rating-value" style="font-size: 15px;">${product.rating} out of 5</span>
        <span class="review-count" style="font-size: 14px;">(${product.reviews ? product.reviews.length : 0} customer ratings)</span>
      </div>

      <div class="detail-price-box">
        <div class="detail-price-amount">$${product.price.toFixed(2)}</div>
        <span class="product-stock-tag ${stockClass}">${stockText}</span>
      </div>

      <h2 class="detail-description-header">About this item</h2>
      <p class="detail-description">${product.description}</p>

      <div class="detail-purchase-controls">
        <div class="qty-control-wrapper">
          <label for="detail-qty-select">Select Quantity</label>
          <select id="detail-qty-select" class="qty-select" ${product.stock === 0 ? 'disabled' : ''}>
            ${qtyOptions}
          </select>
        </div>
        
        <div class="detail-actions">
          <button id="detail-add-cart-btn" class="btn btn-primary btn-large" ${product.stock === 0 ? 'disabled' : ''}>
            ${product.stock === 0 ? 'Out of Stock' : 'Add to Cart'}
          </button>
          
          <button id="detail-toggle-wish-btn" class="btn btn-secondary btn-large" aria-label="${isSaved ? 'Remove from wishlist' : 'Add to wishlist'}">
            <svg viewBox="0 0 24 24" width="20" height="20" fill="${isSaved ? 'currentColor' : 'none'}" stroke="currentColor" stroke-width="2" style="color: ${isSaved ? 'hsl(350, 85%, 55%)' : 'inherit'};">
              <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"/>
            </svg>
          </button>
        </div>
      </div>
    </div>
  `;

  // Bind Actions
  if (product.stock > 0) {
    document.getElementById('detail-add-cart-btn').addEventListener('click', () => {
      const qty = parseInt(document.getElementById('detail-qty-select').value, 10);
      handleAddToCart(product.id, qty);
    });
  }

  document.getElementById('detail-toggle-wish-btn').addEventListener('click', () => {
    handleToggleWishlist(product.id);
    navigateToProductDetail(product.id); // Refresh view details
  });

  // Render Reviews list
  renderReviewsList(product);

  switchView('product-detail-view');
}

// --- Render Reviews ---
function renderReviewsList(product) {
  // Update reviews card summary
  document.getElementById('detail-avg-rating').textContent = product.rating.toFixed(1);
  const count = product.reviews ? product.reviews.length : 0;
  document.getElementById('detail-review-count').textContent = `Based on ${count} review${count === 1 ? '' : 's'}`;
  
  const starsEl = document.getElementById('detail-summary-stars');
  starsEl.innerHTML = getStarsHTML(product.rating);
  starsEl.setAttribute('aria-label', `${product.rating} out of 5 stars`);

  const list = document.getElementById('reviews-list');
  list.innerHTML = '';

  if (!product.reviews || product.reviews.length === 0) {
    list.innerHTML = '<p class="text-muted">No reviews yet. Be the first to share your thoughts!</p>';
    return;
  }

  product.reviews.forEach(review => {
    const item = document.createElement('div');
    item.className = 'review-item';
    item.innerHTML = `
      <div class="review-header">
        <div class="review-user-info">
          <div class="review-avatar">${review.user.slice(0, 2).toUpperCase()}</div>
          <span class="review-username">${review.user}</span>
        </div>
        <span class="review-date">${review.date}</span>
      </div>
      <div class="stars-display review-stars" aria-label="${review.rating} stars">${getStarsHTML(review.rating)}</div>
      <p class="review-comment">${review.comment}</p>
    `;
    list.appendChild(item);
  });
}

// --- Handle Reviews Submission ---
async function handleReviewSubmit(e) {
  e.preventDefault();
  if (!state.detailProductId) return;

  const ratingVal = parseFloat(document.getElementById('review-rating').value);
  const commentVal = document.getElementById('review-comment').value;
  const usernameVal = document.getElementById('review-username').value;

  const payload = {
    rating: ratingVal,
    comment: commentVal,
    user: usernameVal || undefined
  };

  try {
    const res = await fetch(`${API_URL}/products/${state.detailProductId}/reviews`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload)
    });

    const data = await res.json();
    if (res.ok) {
      showToast('Review submitted successfully!', 'success');
      
      // Update state product reference
      const prodIndex = state.products.findIndex(p => p.id === state.detailProductId);
      if (prodIndex !== -1) {
        state.products[prodIndex] = data;
        filterProducts();
      }

      // Reset Form and reload
      document.getElementById('add-review-form').reset();
      navigateToProductDetail(state.detailProductId);
    } else {
      showToast(data.error || 'Failed to submit review', 'error');
    }
  } catch (error) {
    showToast('Failed to submit review', 'error');
  }
}

// --- Add to Cart Logic ---
async function handleAddToCart(productId, quantity) {
  if (state.user) {
    // API backend
    try {
      const res = await fetch(`${API_URL}/cart/items`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ productId, quantity })
      });
      const data = await res.json();
      if (res.ok) {
        showToast('Added to cart successfully!', 'success');
        await loadCart();
        updateHeaderBadges();
      } else {
        showToast(data.error || 'Failed to add item to cart', 'error');
      }
    } catch (e) {
      showToast('Failed to add item to cart', 'error');
    }
  } else {
    // Guest localStorage cart flow
    const product = state.products.find(p => p.id === productId);
    if (!product) return;

    if (state.cart.items.length >= 50 && !state.cart.items.some(item => item.productId === productId)) {
      showToast('Cart soft limit reached (max 50 unique items)', 'error');
      return;
    }

    const existing = state.cart.items.find(item => item.productId === productId);
    if (existing) {
      existing.quantity += quantity;
    } else {
      state.cart.items.push({
        productId,
        quantity,
        priceAtAdd: product.price,
        product: product
      });
    }

    saveGuestCartLocal();
    showToast('Added to cart successfully!', 'success');
    updateHeaderBadges();
  }
}

// --- Shopping Cart Drawer Rendering ---
function renderCart() {
  const container = document.getElementById('cart-items-container');
  const subtotalEl = document.getElementById('cart-subtotal');
  const checkoutBtn = document.getElementById('proceed-checkout-btn');
  const clearBtn = document.getElementById('clear-cart-btn');

  container.innerHTML = '';
  
  // Refresh guest references with actual product changes
  if (!state.user) {
    state.cart.items.forEach(item => {
      const p = state.products.find(prod => prod.id === item.productId);
      if (p) item.product = p;
    });
    // Filter items without product reference
    state.cart.items = state.cart.items.filter(item => item.product);
  }

  const items = state.cart.items || [];

  if (items.length === 0) {
    container.innerHTML = `
      <div class="empty-state">
        <h2>Your Cart is empty</h2>
        <p>Browse products and add items to your shopping cart.</p>
        <button id="cart-continue-shopping" class="btn btn-primary">Continue Shopping</button>
      </div>
    `;
    subtotalEl.textContent = '$0.00';
    checkoutBtn.disabled = true;
    clearBtn.style.display = 'none';

    document.getElementById('cart-continue-shopping').addEventListener('click', () => {
      closeDrawer('cart-drawer');
      switchView('products-view');
    });
    return;
  }

  checkoutBtn.disabled = false;
  clearBtn.style.display = 'block';

  let totalSum = 0;

  items.forEach(item => {
    const product = item.product;
    if (!product) return;

    const lineTotal = product.price * item.quantity;
    totalSum += lineTotal;

    const row = document.createElement('div');
    row.className = 'cart-item';
    row.innerHTML = `
      <img src="${product.image}" alt="${product.title}" class="cart-item-image">
      <div class="cart-item-info">
        <h3 class="cart-item-title">${product.title}</h3>
        <span class="cart-item-price">$${product.price.toFixed(2)}</span>
        
        <div class="cart-item-controls">
          <div class="quantity-adjuster">
            <button class="qty-btn dec-qty-btn" aria-label="Decrease quantity">-</button>
            <span class="qty-val" aria-live="polite">${item.quantity}</span>
            <button class="qty-btn inc-qty-btn" aria-label="Increase quantity" ${item.quantity >= product.stock ? 'disabled' : ''}>+</button>
          </div>
          <button class="remove-item-btn" aria-label="Remove ${product.title} from cart">Remove</button>
        </div>
      </div>
    `;

    // Bind Adjusters
    row.querySelector('.dec-qty-btn').addEventListener('click', () => handleUpdateQuantity(product.id, item.quantity - 1));
    row.querySelector('.inc-qty-btn').addEventListener('click', () => handleUpdateQuantity(product.id, item.quantity + 1));
    row.querySelector('.remove-item-btn').addEventListener('click', () => handleRemoveCartItem(product.id));

    container.appendChild(row);
  });

  subtotalEl.textContent = `$${totalSum.toFixed(2)}`;
}

// --- Cart Actions Functions ---
async function handleUpdateQuantity(productId, newQty) {
  const product = state.products.find(p => p.id === productId);
  if (!product) return;

  if (newQty > product.stock) {
    showToast(`Sorry, only ${product.stock} items available in stock`, 'error');
    return;
  }

  if (state.user) {
    try {
      const res = await fetch(`${API_URL}/cart/items/${productId}`, {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ quantity: newQty })
      });
      if (res.ok) {
        await loadCart();
        renderCart();
        updateHeaderBadges();
      }
    } catch (e) {
      showToast('Failed to update cart quantity', 'error');
    }
  } else {
    // Guest
    if (newQty <= 0) {
      state.cart.items = state.cart.items.filter(item => item.productId !== productId);
    } else {
      const item = state.cart.items.find(item => item.productId === productId);
      if (item) item.quantity = newQty;
    }
    saveGuestCartLocal();
    renderCart();
    updateHeaderBadges();
  }
}

async function handleRemoveCartItem(productId) {
  if (state.user) {
    try {
      const res = await fetch(`${API_URL}/cart/items/${productId}`, {
        method: 'DELETE'
      });
      if (res.ok) {
        showToast('Item removed from cart', 'success');
        await loadCart();
        renderCart();
        updateHeaderBadges();
      }
    } catch (e) {
      showToast('Failed to remove item', 'error');
    }
  } else {
    state.cart.items = state.cart.items.filter(item => item.productId !== productId);
    saveGuestCartLocal();
    showToast('Item removed from cart', 'success');
    renderCart();
    updateHeaderBadges();
  }
}

async function handleClearCart() {
  if (state.user) {
    try {
      const res = await fetch(`${API_URL}/cart`, { method: 'DELETE' });
      if (res.ok) {
        showToast('Shopping cart cleared', 'success');
        await loadCart();
        renderCart();
        updateHeaderBadges();
      }
    } catch (e) {
      showToast('Failed to clear cart', 'error');
    }
  } else {
    state.cart.items = [];
    saveGuestCartLocal();
    showToast('Shopping cart cleared', 'success');
    renderCart();
    updateHeaderBadges();
  }
}

// --- Toggle Wishlist ---
async function handleToggleWishlist(productId) {
  if (state.user) {
    const isSaved = state.wishlist.some(w => w.id === productId);
    const method = isSaved ? 'DELETE' : 'POST';
    const url = isSaved ? `${API_URL}/auth/wishlist/${productId}` : `${API_URL}/auth/wishlist`;
    const payload = isSaved ? {} : { productId };

    try {
      const res = await fetch(url, {
        method,
        headers: { 'Content-Type': 'application/json' },
        body: method === 'POST' ? JSON.stringify(payload) : undefined
      });
      const data = await res.json();
      if (res.ok) {
        state.wishlist = data;
        showToast(isSaved ? 'Removed from wishlist' : 'Added to wishlist!', 'success');
        updateHeaderBadges();
        if (state.activeView === 'wishlist-view') renderWishlist();
        if (state.activeView === 'products-view') renderProductsList();
      }
    } catch (e) {
      showToast('Wishlist operation failed', 'error');
    }
  } else {
    // Guest Local Wishlist
    const existingIndex = state.wishlist.findIndex(item => item.id === productId);
    if (existingIndex !== -1) {
      state.wishlist.splice(existingIndex, 1);
      showToast('Removed from wishlist', 'success');
    } else {
      const product = state.products.find(p => p.id === productId);
      if (product) {
        state.wishlist.push(product);
        showToast('Added to wishlist!', 'success');
      }
    }
    saveGuestWishlistLocal();
    updateHeaderBadges();
    if (state.activeView === 'wishlist-view') renderWishlist();
    if (state.activeView === 'products-view') renderProductsList();
  }
}

// --- Render 3: Wishlist View ---
function renderWishlist() {
  const grid = document.getElementById('wishlist-items-grid');
  const emptyState = document.getElementById('wishlist-empty-state');
  grid.innerHTML = '';

  if (state.wishlist.length === 0) {
    emptyState.style.display = 'block';
    return;
  }

  emptyState.style.display = 'none';

  state.wishlist.forEach(product => {
    const card = document.createElement('div');
    card.className = 'product-card';
    card.innerHTML = `
      <button class="wishlist-heart-btn active" data-id="${product.id}" aria-label="Remove from wishlist">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
          <path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"/>
        </svg>
      </button>
      <div class="product-image-wrapper" onclick="navigateToProductDetail('${product.id}')">
        <img src="${product.image}" alt="${product.title}" class="product-img">
      </div>
      <div class="product-info" onclick="navigateToProductDetail('${product.id}')">
        <span class="product-category">${product.category}</span>
        <h3 class="product-title">${product.title}</h3>
        <div class="price-container">
          <span class="price-currency">$</span>
          <span class="price-amount">${product.price.toFixed(2)}</span>
        </div>
      </div>
      <div class="card-actions">
        <button class="btn btn-primary btn-block add-to-cart-btn" data-id="${product.id}" ${product.stock === 0 ? 'disabled' : ''}>
          Add to Cart
        </button>
      </div>
    `;

    card.querySelector('.wishlist-heart-btn').addEventListener('click', () => handleToggleWishlist(product.id));
    card.querySelector('.add-to-cart-btn').addEventListener('click', () => handleAddToCart(product.id, 1));
    grid.appendChild(card);
  });
}

// --- Render 4: Orders History ---
function renderOrders() {
  const container = document.getElementById('orders-list');
  const emptyState = document.getElementById('orders-empty-state');
  container.innerHTML = '';

  if (state.orders.length === 0) {
    emptyState.style.display = 'block';
    return;
  }

  emptyState.style.display = 'none';

  state.orders.forEach(order => {
    const card = document.createElement('div');
    card.className = 'order-card';
    
    let itemsHTML = '';
    order.items.forEach(item => {
      itemsHTML += `
        <div class="order-product-row">
          <div>
            <span class="order-product-name">${item.title}</span>
            <span class="order-product-qty">x ${item.quantity}</span>
          </div>
          <span class="font-bold">$${item.total.toFixed(2)}</span>
        </div>
      `;
    });

    card.innerHTML = `
      <div class="order-card-header">
        <div class="order-header-block">
          <span class="order-header-label">Order Placed</span>
          <span class="order-header-val">${order.date}</span>
        </div>
        <div class="order-header-block">
          <span class="order-header-label">Total Amount</span>
          <span class="order-header-val font-bold">$${order.total.toFixed(2)}</span>
        </div>
        <div class="order-header-block">
          <span class="order-header-label">Ship To</span>
          <span class="order-header-val">${order.fullName}</span>
        </div>
        <div class="order-header-block order-id-block">
          <span class="order-header-label">Order ID</span>
          <span class="order-header-val" style="font-family: monospace;">${order.id}</span>
        </div>
      </div>
      <div class="order-card-body">
        <div class="order-status-banner">
          <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor"><path d="M20 8h-3V4H3c-1.1 0-2 .9-2 2v11h2c0 1.66 1.34 3 3 3s3-1.34 3-3h6c0 1.66 1.34 3 3 3s3-1.34 3-3h2v-5l-3-4zM6 18.5c-.83 0-1.5-.67-1.5-1.5s.67-1.5 1.5-1.5 1.5.67 1.5 1.5-.67 1.5-1.5 1.5zm12 0c-.83 0-1.5-.67-1.5-1.5s.67-1.5 1.5-1.5 1.5.67 1.5 1.5-.67 1.5-1.5 1.5zm1.2-5.5h-2.2V9h2.2l1.8 3v1z"/></svg>
          Status: ${order.status} (Estimated Delivery: ${order.estimatedDelivery})
        </div>
        <div class="order-items-grid">
          ${itemsHTML}
        </div>
      </div>
    `;

    container.appendChild(card);
  });
}

// --- Render 5: Checkout Summary ---
function renderCheckoutSummary() {
  const container = document.getElementById('checkout-items-list');
  const totalEl = document.getElementById('checkout-total-price');
  container.innerHTML = '';

  const items = state.cart.items || [];
  let sum = 0;

  items.forEach(item => {
    const product = item.product;
    if (!product) return;

    const lineTotal = product.price * item.quantity;
    sum += lineTotal;

    const row = document.createElement('div');
    row.className = 'checkout-summary-item';
    row.innerHTML = `
      <span>${product.title.slice(0, 40)}... (x${item.quantity})</span>
      <span class="font-bold">$${lineTotal.toFixed(2)}</span>
    `;
    container.appendChild(row);
  });

  totalEl.textContent = `$${sum.toFixed(2)}`;
}

// --- Form Validation & Checkout ---
function setupCheckoutValidation() {
  const form = document.getElementById('checkout-form');
  const submitBtn = document.getElementById('checkout-submit-btn');
  const inputs = form.querySelectorAll('input');

  const validators = {
    email: (val) => {
      if (!val || !val.includes('@') || !val.includes('.')) return 'Invalid email address format';
      return '';
    },
    fullName: (val) => {
      if (!val || val.trim().length < 2) return 'Full Name must be at least 2 characters';
      return '';
    },
    address: (val) => {
      if (!val || val.trim().length < 10) return 'Street address must be at least 10 characters';
      return '';
    },
    city: (val) => {
      if (!val || !val.trim()) return 'City is a required field';
      return '';
    },
    state: (val) => {
      if (!val || !val.trim()) return 'State is a required field';
      return '';
    },
    zip: (val) => {
      if (!val || !/^\d+$/.test(val)) return 'ZIP code must be numeric only';
      return '';
    },
    cardNumber: (val) => {
      const clean = val.replace(/\s/g, '');
      if (!clean || !/^\d{16}$/.test(clean)) return 'Credit card number must be 16 digits';
      return '';
    },
    expiryDate: (val) => {
      if (!val || !/^\d{2}\/\d{2}$/.test(val)) return 'Expiry must be in MM/YY format';
      return '';
    },
    cvv: (val) => {
      if (!val || !/^\d{3,4}$/.test(val)) return 'CVV must be 3 or 4 digits';
      return '';
    }
  };

  // Credit Card formatting / masking
  const cardInput = document.getElementById('checkout-card');
  cardInput.addEventListener('input', (e) => {
    let clean = e.target.value.replace(/\D/g, '').slice(0, 16);
    let parts = [];
    for (let i = 0; i < clean.length; i += 4) {
      parts.push(clean.slice(i, i + 4));
    }
    e.target.value = parts.join(' ');
  });

  // Expiry formatting / masking
  const expiryInput = document.getElementById('checkout-expiry');
  expiryInput.addEventListener('input', (e) => {
    let clean = e.target.value.replace(/\D/g, '').slice(0, 4);
    if (clean.length > 2) {
      e.target.value = clean.slice(0, 2) + '/' + clean.slice(2);
    } else {
      e.target.value = clean;
    }
  });

  // Validate field on Blur (loss of focus)
  inputs.forEach(input => {
    input.addEventListener('blur', () => {
      validateField(input);
      checkFormValidity();
    });
    
    input.addEventListener('input', () => {
      // Clear error immediately if typing, but do not announce success yet
      if (input.classList.contains('invalid')) {
        validateField(input);
      }
      checkFormValidity();
    });
  });

  function validateField(input) {
    const name = input.name;
    const validator = validators[name];
    if (!validator) return true;

    const errorMsg = validator(input.value);
    const errorEl = document.getElementById(`${name === 'cardNumber' ? 'card' : name === 'expiryDate' ? 'expiry' : name}-error`);

    if (errorMsg) {
      input.classList.add('invalid');
      if (errorEl) {
        errorEl.textContent = errorMsg;
        errorEl.setAttribute('role', 'alert');
      }
      return false;
    } else {
      input.classList.remove('invalid');
      if (errorEl) {
        errorEl.textContent = '';
        errorEl.removeAttribute('role');
      }
      return true;
    }
  }

  function checkFormValidity() {
    let isValid = true;
    inputs.forEach(input => {
      const name = input.name;
      const validator = validators[name];
      if (validator && validator(input.value) !== '') {
        isValid = false;
      }
    });
    submitBtn.disabled = !isValid;
  }

  // Handle Form Submission
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    
    let isFormValid = true;
    inputs.forEach(input => {
      if (!validateField(input)) isFormValid = false;
    });

    if (!isFormValid) {
      showToast('Please correct validation errors first', 'error');
      return;
    }

    // Assemble payload
    const formData = new FormData(form);
    const payload = {};
    formData.forEach((value, key) => {
      payload[key] = value;
    });

    if (state.user) {
      try {
        const res = await fetch(`${API_URL}/orders`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(payload)
        });
        
        const data = await res.json();
        if (res.ok) {
          showToast('Checkout completed successfully!', 'success');
          
          // Clear cart local state & badges
          await loadCart();
          await loadProducts(); // Reload to capture stock reductions
          await loadOrders();
          
          showConfirmation(data);
        } else {
          showToast(data.error || 'Failed to place order', 'error');
        }
      } catch (err) {
        showToast('Checkout connection error', 'error');
      }
    } else {
      // Guest Checkout simulation
      const orderId = 'G' + Math.random().toString(36).substr(2, 9).toUpperCase();
      const estimatedDelivery = new Date();
      estimatedDelivery.setDate(estimatedDelivery.getDate() + 4);

      // Decrement local inventory mock
      state.cart.items.forEach(item => {
        const product = state.products.find(p => p.id === item.productId);
        if (product) product.stock = Math.max(0, product.stock - item.quantity);
      });

      const totalSum = state.cart.items.reduce((sum, item) => sum + (item.product.price * item.quantity), 0);

      const guestOrder = {
        id: orderId,
        fullName: payload.fullName,
        email: payload.email.toLowerCase(),
        estimatedDelivery: estimatedDelivery.toISOString().split('T')[0],
        total: totalSum,
        items: state.cart.items.map(item => ({
          title: item.product.title,
          quantity: item.quantity,
          total: item.product.price * item.quantity
        }))
      };

      // Clear local cart
      state.cart.items = [];
      saveGuestCartLocal();
      updateHeaderBadges();

      showToast('Checkout completed successfully!', 'success');
      showConfirmation(guestOrder);
    }
  });
}

// --- Render 6: Order Confirmation ---
function showConfirmation(order) {
  document.getElementById('confirm-order-id').textContent = order.id;
  document.getElementById('confirm-delivery').textContent = order.estimatedDelivery;
  document.getElementById('confirm-recipient').textContent = order.fullName;
  document.getElementById('confirm-email').textContent = order.email;
  document.getElementById('confirm-total-price').textContent = `$${order.total.toFixed(2)}`;

  const confirmItemsEl = document.getElementById('confirm-items');
  confirmItemsEl.innerHTML = '';

  order.items.forEach(item => {
    const row = document.createElement('div');
    row.className = 'summary-line';
    row.innerHTML = `
      <span>${item.title.slice(0, 45)}... (x${item.quantity})</span>
      <span>$${item.total.toFixed(2)}</span>
    `;
    confirmItemsEl.appendChild(row);
  });

  // Setup click to copy ID
  const copyBtn = document.getElementById('copy-order-id-btn');
  copyBtn.addEventListener('click', () => {
    navigator.clipboard.writeText(order.id).then(() => {
      showToast('Order ID copied to clipboard!', 'success');
    });
  });

  document.getElementById('checkout-form').reset();
  switchView('confirmation-view');
}

// --- Render 7: Admin Inventory ---
function renderAdminInventory() {
  const tbody = document.getElementById('inventory-table-body');
  tbody.innerHTML = '';

  state.products.forEach(product => {
    const tr = document.createElement('tr');
    tr.innerHTML = `
      <td><img src="${product.image}" alt="" class="inventory-thumbnail"></td>
      <td style="font-family: monospace; font-weight: bold;">${product.id}</td>
      <td style="font-weight: 500;">${product.title.slice(0, 50)}...</td>
      <td>${product.category}</td>
      <td class="font-bold">$${product.price.toFixed(2)}</td>
      <td class="font-bold" id="admin-stock-val-${product.id}">${product.stock}</td>
      <td>
        <form class="admin-stock-adjust-form" data-id="${product.id}">
          <label for="stock-input-${product.id}" class="sr-only">Stock for ${product.id}</label>
          <input type="number" id="stock-input-${product.id}" class="stock-adjust-input" min="0" value="${product.stock}" required>
          <button type="submit" class="btn btn-secondary btn-sm">Update</button>
        </form>
      </td>
    `;

    tr.querySelector('.admin-stock-adjust-form').addEventListener('submit', async (e) => {
      e.preventDefault();
      const input = tr.querySelector('.stock-adjust-input');
      const newStock = parseInt(input.value, 10);
      
      try {
        const res = await fetch(`${API_URL}/products/${product.id}/stock`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ stock: newStock })
        });
        const data = await res.json();
        if (res.ok) {
          showToast(`Stock updated for ${product.id} successfully!`, 'success');
          // Update local state product reference
          const prodIndex = state.products.findIndex(p => p.id === product.id);
          if (prodIndex !== -1) {
            state.products[prodIndex] = data;
            filterProducts();
          }
          document.getElementById(`admin-stock-val-${product.id}`).textContent = data.stock;
        } else {
          showToast(data.error || 'Failed to update stock', 'error');
        }
      } catch (err) {
        showToast('Admin update stock network error', 'error');
      }
    });

    tbody.appendChild(tr);
  });
}

// --- Header Badges Update ---
function updateHeaderBadges() {
  const items = state.cart.items || [];
  const qtySum = items.reduce((sum, item) => sum + item.quantity, 0);

  const cartBadge = document.getElementById('cart-badge');
  cartBadge.textContent = qtySum;
  
  // Set accessibility announcer
  const cartNavBtn = document.getElementById('cart-nav-btn');
  cartNavBtn.setAttribute('aria-label', `Shopping Cart, ${qtySum} item${qtySum === 1 ? '' : 's'}`);
  
  const wishlistBadge = document.getElementById('wishlist-badge');
  wishlistBadge.textContent = state.wishlist.length;

  // Update screen reader announcer
  document.getElementById('sr-announcer').textContent = `Cart count updated to ${qtySum} items, Wishlist count updated to ${state.wishlist.length} items.`;
}

// --- Auth Manager Operations ---

function toggleAuthTabs(tab) {
  const signinBtn = document.getElementById('tab-signin');
  const signupBtn = document.getElementById('tab-signup');
  const signinPanel = document.getElementById('signin-panel');
  const signupPanel = document.getElementById('signup-panel');

  if (tab === 'signin') {
    signinBtn.classList.add('active');
    signinBtn.setAttribute('aria-selected', 'true');
    signupBtn.classList.remove('active');
    signupBtn.setAttribute('aria-selected', 'false');
    signinPanel.style.display = 'block';
    signupPanel.style.display = 'none';
  } else {
    signupBtn.classList.add('active');
    signupBtn.setAttribute('aria-selected', 'true');
    signinBtn.classList.remove('active');
    signinBtn.setAttribute('aria-selected', 'false');
    signupPanel.style.display = 'block';
    signinPanel.style.display = 'none';
  }
}

async function handleSignIn(e) {
  e.preventDefault();
  const email = document.getElementById('signin-email').value;
  const password = document.getElementById('signin-password').value;

  try {
    const res = await fetch(`${API_URL}/auth/signin`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email, password })
    });
    
    const data = await res.json();
    if (res.ok) {
      state.user = data.user;
      showToast(`Welcome back, ${state.user.name}!`, 'success');
      
      // Perform state reload sync
      await loadCart();
      await loadWishlist();
      await loadOrders();
      
      renderAuthDrawer();
      updateHeaderBadges();
      closeDrawer('auth-drawer');
      
      document.getElementById('signin-form').reset();
    } else {
      showToast(data.error || 'Invalid credentials', 'error');
    }
  } catch (err) {
    showToast('Sign in connection failed', 'error');
  }
}

async function handleSignUp(e) {
  e.preventDefault();
  const name = document.getElementById('signup-name').value;
  const email = document.getElementById('signup-email').value;
  const password = document.getElementById('signup-password').value;

  try {
    const res = await fetch(`${API_URL}/auth/signup`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name, email, password })
    });
    
    const data = await res.json();
    if (res.ok) {
      state.user = data.user;
      showToast(`Welcome to Amazon Clone, ${state.user.name}!`, 'success');
      
      // Merge guest cart to backend if items exist
      if (state.cart.items.length > 0) {
        for (const item of state.cart.items) {
          await fetch(`${API_URL}/cart/items`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ productId: item.productId, quantity: item.quantity })
          });
        }
      }

      await loadCart();
      await loadWishlist();
      await loadOrders();
      
      renderAuthDrawer();
      updateHeaderBadges();
      closeDrawer('auth-drawer');
      
      document.getElementById('signup-form').reset();
    } else {
      showToast(data.error || 'Failed to create account', 'error');
    }
  } catch (err) {
    showToast('Sign up connection failed', 'error');
  }
}

async function handleSignOut(e) {
  try {
    const res = await fetch(`${API_URL}/auth/signout`, { method: 'POST' });
    if (res.ok) {
      state.user = null;
      state.wishlist = [];
      state.orders = [];
      
      // Reset cart to empty
      state.cart = { items: [] };
      saveGuestCartLocal();
      saveGuestWishlistLocal();

      showToast('Signed out successfully', 'success');
      renderAuthDrawer();
      updateHeaderBadges();
      closeDrawer('auth-drawer');
      switchView('products-view');
    }
  } catch (e) {
    showToast('Failed to sign out', 'error');
  }
}

function renderAuthDrawer() {
  const signedInState = document.getElementById('auth-signed-in-state');
  const signedOutState = document.getElementById('auth-signed-out-state');
  const navBtnLine1 = document.querySelector('#auth-nav-btn .nav-line-1');
  const navBtnLine2 = document.querySelector('#auth-nav-btn .nav-line-2');

  if (state.user) {
    signedInState.style.display = 'block';
    signedOutState.style.display = 'none';

    document.getElementById('user-profile-name').textContent = state.user.name;
    document.getElementById('user-profile-email').textContent = state.user.email;
    document.getElementById('user-avatar-initials').textContent = state.user.name.slice(0, 2).toUpperCase();

    navBtnLine1.textContent = `Hello, ${state.user.name.split(' ')[0]}`;
    navBtnLine2.textContent = 'Account & Lists';

    // Auth drawer menu lists links
    document.getElementById('menu-orders-btn').onclick = () => {
      closeDrawer('auth-drawer');
      switchView('orders-view');
    };
    document.getElementById('menu-wishlist-btn').onclick = () => {
      closeDrawer('auth-drawer');
      switchView('wishlist-view');
    };
  } else {
    signedInState.style.display = 'none';
    signedOutState.style.display = 'block';

    navBtnLine1.textContent = 'Hello, Sign in';
    navBtnLine2.textContent = 'Account & Lists';
  }
}

// --- Notification Toasts Generator ---
function showToast(message, type = 'success') {
  const container = document.getElementById('toast-container');
  const toast = document.createElement('div');
  toast.className = `toast toast-${type}`;
  toast.innerHTML = `
    <span>${message}</span>
    <button class="toast-close" aria-label="Close message">&times;</button>
  `;
  
  toast.querySelector('.toast-close').addEventListener('click', () => {
    toast.remove();
  });

  container.appendChild(toast);

  // Auto remove after 4 seconds
  setTimeout(() => {
    toast.remove();
  }, 4000);
}
