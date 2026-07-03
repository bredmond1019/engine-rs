# E2E Template — UI Form Submit

Test a form submit flow in the browser using Playwright (or playwright-mcp). Replace all
`{{PLACEHOLDER}}` tokens before running.

---

## Setup

- **App URL:** `{{BASE_URL}}/{{FORM_PAGE_PATH}}` (e.g. `http://localhost:3000/signup`)
- **Tool:** Playwright (headless) or playwright-mcp for agent-driven browser control.

## Test Cases

### 1. Happy path — valid submit

1. Navigate to `{{BASE_URL}}/{{FORM_PAGE_PATH}}`.
2. Assert the form is visible (look for `{{FORM_SELECTOR}}` — e.g. `form[data-testid="signup"]`).
3. Fill `{{FIELD_1_SELECTOR}}` with `{{FIELD_1_VALID_VALUE}}`.
4. Fill `{{FIELD_2_SELECTOR}}` with `{{FIELD_2_VALID_VALUE}}`.
5. Click the submit button (`{{SUBMIT_SELECTOR}}`).
6. **Assert success state:**
   - URL changes to `{{SUCCESS_URL}}` OR
   - A success banner/toast appears matching `{{SUCCESS_MESSAGE_TEXT}}`.
7. *(Optional)* Take a screenshot: `screenshot → tests/e2e/screenshots/form-success.png`.

---

### 2. Validation error — required field empty

1. Navigate to `{{BASE_URL}}/{{FORM_PAGE_PATH}}`.
2. Leave `{{REQUIRED_FIELD_SELECTOR}}` empty.
3. Fill all other fields with valid values.
4. Click submit.
5. **Assert error state:**
   - Form does NOT navigate away (URL unchanged).
   - An inline error message appears near `{{REQUIRED_FIELD_SELECTOR}}`.
   - The error message contains `{{REQUIRED_FIELD_ERROR_TEXT}}` (or any non-empty error text).
6. *(Optional)* Screenshot: `tests/e2e/screenshots/form-validation-error.png`.

---

### 3. Validation error — invalid format

1. Navigate to `{{BASE_URL}}/{{FORM_PAGE_PATH}}`.
2. Fill `{{FORMAT_FIELD_SELECTOR}}` with `{{INVALID_FORMAT_VALUE}}` (e.g. `notanemail` for an
   email field).
3. Fill remaining fields validly.
4. Click submit.
5. **Assert:**
   - Form does not navigate.
   - Error visible near `{{FORMAT_FIELD_SELECTOR}}`.

---

### 4. Server error handling

1. Simulate a server 500 (intercept the submit network request if using Playwright's
   `page.route()`, or bring up a mock server that returns 500).
2. Submit the form with valid data.
3. **Assert:**
   - A user-visible error message appears (not a raw stack trace or blank page).
   - The form does not silently fail.

---

## Notes

- Use `page.screenshot()` to capture evidence for any failing assertion.
- If using playwright-mcp, replace direct Playwright calls with the equivalent MCP tool actions.
- Keep screenshots in `tests/e2e/screenshots/` and add that directory to `.gitignore` if they
  should not be committed.
