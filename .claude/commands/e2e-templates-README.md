# E2E Test Templates

These are **templates, not auto-run tests**. Each file describes a test scenario in structured
markdown that an agent can follow to write or execute E2E tests for a feature.

## How to use

1. Copy the relevant template to `tests/e2e/test_<feature>.md` (or adapt to `.py` / `.spec.ts`)
   in your project.
2. Replace every `{{PLACEHOLDER}}` with your project's actual values (endpoint URLs, field names,
   auth tokens, etc.).
3. Run the test via your project's test runner, or invoke it as a Claude Code command if you
   want the agent to drive it.

## Available templates

| File | Covers |
|---|---|
| `test_crud_api.md` | Create / read / update / delete against a REST endpoint |
| `test_ui_form.md` | Form submit flow in the browser (Playwright) |
| `test_error_handling.md` | Error surfaces — validation, 500, network timeout |
| `test_auth_gate.md` | Protected route / endpoint rejects unauthenticated requests |

## Integrating with the SDLC pipeline

If you want E2E tests to run as part of the `sdlc-block` back-half, set
`block.verify: "consolidated+review"` in `planning/harness.json` and add your E2E command to
`validation.checks[]`. The consolidated review stage will run it over the integrated tree.

## Placeholders

All templates use `{{PLACEHOLDER}}` tokens. Common ones:

| Token | Replace with |
|---|---|
| `{{BASE_URL}}` | Your API or app base URL (e.g. `http://localhost:8000`) |
| `{{RESOURCE}}` | The resource name (e.g. `users`, `posts`) |
| `{{AUTH_TOKEN}}` | A valid bearer token for authenticated requests |
| `{{FIELD_NAME}}` | A required field that triggers validation |
| `{{LOGIN_URL}}` | The login/auth endpoint or page URL |
| `{{PROTECTED_URL}}` | A URL that requires authentication |
