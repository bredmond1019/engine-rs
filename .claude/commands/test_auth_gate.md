# E2E Template — Auth Gate

Test that protected routes and endpoints correctly reject unauthenticated/unauthorized requests
and pass through with valid credentials. Replace all `{{PLACEHOLDER}}` tokens before running.

---

## Setup

- **Login URL / endpoint:** `{{LOGIN_URL}}` (e.g. `http://localhost:8000/auth/login`)
- **Protected resource:** `{{PROTECTED_URL}}` (e.g. `http://localhost:8000/api/me`)
- **Valid credentials:** `{{TEST_USERNAME}}` / `{{TEST_PASSWORD}}`
- **Admin-only resource (if applicable):** `{{ADMIN_URL}}`

---

## API Auth Gate Tests

### 1. Unauthenticated request — 401

```
GET {{PROTECTED_URL}}
```
*(No Authorization header)*

**Assert:**
- Status: `401 Unauthorized`.
- Body does NOT contain protected data.

---

### 2. Invalid / expired token — 401

```
GET {{PROTECTED_URL}}
Authorization: Bearer invalid-token-xyz
```

**Assert:**
- Status: `401 Unauthorized`.
- Body does NOT contain protected data.
- Body does NOT expose token internals or signing key info.

---

### 3. Valid token — 200

```
POST {{LOGIN_URL}}
Content-Type: application/json

{"username": "{{TEST_USERNAME}}", "password": "{{TEST_PASSWORD}}"}
```

Store the returned token as `VALID_TOKEN`.

```
GET {{PROTECTED_URL}}
Authorization: Bearer {{VALID_TOKEN}}
```

**Assert:**
- Status: `200 OK`.
- Body contains the expected protected data.

---

### 4. Insufficient role / permissions — 403

*(Skip if the project has no role-based access control)*

Use a token for a low-privilege user (`{{LOW_PRIV_TOKEN}}`):

```
GET {{ADMIN_URL}}
Authorization: Bearer {{LOW_PRIV_TOKEN}}
```

**Assert:**
- Status: `403 Forbidden`.
- Body does NOT contain admin data.

---

## UI Auth Gate Tests

### 5. Redirect unauthenticated browser user

1. Clear cookies / local storage (start fresh, not logged in).
2. Navigate directly to `{{PROTECTED_URL}}`.
3. **Assert:**
   - Browser is redirected to `{{LOGIN_URL}}` (or a login page).
   - The protected content is NOT visible before redirect.

---

### 6. Access after login

1. Navigate to `{{LOGIN_URL}}`.
2. Enter `{{TEST_USERNAME}}` and `{{TEST_PASSWORD}}` and submit.
3. **Assert:**
   - Browser is redirected to `{{POST_LOGIN_URL}}` (the intended destination or dashboard).
   - Protected content is now visible.

---

### 7. Logout clears access

1. While logged in, navigate to `{{PROTECTED_URL}}` — assert accessible.
2. Trigger logout (click logout button or call `{{LOGOUT_URL}}`).
3. Navigate again to `{{PROTECTED_URL}}`.
4. **Assert:**
   - User is redirected to login (not served the protected page from cache).

---

## Pass Criteria

| Check | Expected |
|---|---|
| Unauthenticated API → 401 | Yes |
| Invalid token → 401, no data leak | Yes |
| Valid token → 200, data returned | Yes |
| Insufficient role → 403 | Yes (if RBAC applies) |
| Unauthenticated browser → redirect to login | Yes |
| Post-login → protected content visible | Yes |
| Post-logout → access revoked | Yes |
