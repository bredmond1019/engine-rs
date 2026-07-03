# E2E Template — Error Handling

Test that the UI and API surface errors correctly and do not expose raw internals. Replace all
`{{PLACEHOLDER}}` tokens before running.

---

## Principle

For every error path, assert two things:
1. The right HTTP status / UI state is returned.
2. No raw stack trace, internal path, or unmasked secret leaks to the client.

---

## API Error Tests

### 1. Validation error (400 / 422)

Trigger: send a request with a missing or malformed required field.

```
POST {{BASE_URL}}/{{ENDPOINT}}
Content-Type: application/json

{ "{{FIELD_NAME}}": null }
```

**Assert:**
- Status: `400` or `422`.
- Body contains a human-readable `detail` / `message` / `errors` field.
- Body does NOT contain a Python traceback, SQL query, or file path.
- Body does NOT echo back internal variable names.

---

### 2. Server error (500)

Trigger: call an endpoint designed to fail (or mock via test setup / network intercept).

```
GET {{BASE_URL}}/{{ERROR_TRIGGER_ENDPOINT}}
```

**Assert:**
- Status: `500 Internal Server Error`.
- Body is a generic error message (e.g. `{"detail": "Internal server error"}`).
- Body does NOT contain a stack trace.
- Body does NOT contain file system paths.
- Response time < `{{TIMEOUT_MS}}` ms (server didn't hang).

---

### 3. Network timeout

Trigger: configure the client to use a very short timeout (e.g. 50 ms), or use a mock server
with a delayed response.

**Assert (client-side / UI):**
- The UI shows a user-visible error (e.g. "Request timed out. Please try again.").
- The UI does not hang indefinitely.
- The UI does not show a raw `ECONNRESET` or `TimeoutError` stack trace.

---

## UI Error Tests

### 4. Form validation — user-visible error message

Trigger: submit a form with an invalid or missing field (see `test_ui_form.md` step 2).

**Assert:**
- An error message is visible near the invalid field.
- The error is in plain language (not a code reference like `ValidationError at line 42`).
- The page does not crash (no blank white screen, no unhandled promise rejection banner).

---

### 5. 404 — unknown route

Navigate to a URL that does not exist: `{{BASE_URL}}/does-not-exist-xyz`

**Assert:**
- HTTP status (or equivalent client-side route): 404.
- A friendly "not found" message is shown.
- No raw error output visible to the user.

---

### 6. Unauthorized access — error message, not crash

Navigate to a protected page without being logged in: `{{PROTECTED_URL}}`

**Assert:**
- User is redirected to login OR shown a "you must be logged in" message.
- No raw `401` JSON is rendered directly in the browser.
- No unhandled exception crashes the page.

---

## Pass Criteria

| Check | Expected |
|---|---|
| 4xx responses have readable error bodies | Yes |
| 500 responses hide stack traces | Yes |
| UI shows errors in plain language | Yes |
| No raw internals (paths, traces, SQL) visible | Yes |
| Timeout results in a user-friendly message | Yes |
