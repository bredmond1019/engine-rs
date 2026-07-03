# E2E Template — CRUD API

Test create / read / update / delete against a REST API endpoint. Adapt for FastAPI, Express,
Rails, or any HTTP backend. Replace all `{{PLACEHOLDER}}` tokens before running.

---

## Setup

- **Base URL:** `{{BASE_URL}}` (e.g. `http://localhost:8000`)
- **Resource:** `{{RESOURCE}}` (e.g. `users`, `posts`, `items`)
- **Auth:** Bearer token `{{AUTH_TOKEN}}` (or remove the `Authorization` header if the endpoint
  is public)

## Test Cases

### 1. Create (POST) — happy path

```
POST {{BASE_URL}}/{{RESOURCE}}
Authorization: Bearer {{AUTH_TOKEN}}
Content-Type: application/json

{
  "{{FIELD_NAME}}": "{{TEST_VALUE}}"
}
```

**Assert:**
- Status: `201 Created`
- Response body contains a generated `id` field.
- Response body echoes back the submitted `{{FIELD_NAME}}`.

Store the returned `id` as `CREATED_ID` for subsequent steps.

---

### 2. Read (GET) — fetch the created resource

```
GET {{BASE_URL}}/{{RESOURCE}}/{{CREATED_ID}}
Authorization: Bearer {{AUTH_TOKEN}}
```

**Assert:**
- Status: `200 OK`
- Body `id` == `CREATED_ID`.
- Body `{{FIELD_NAME}}` matches what was submitted in step 1.

---

### 3. Update (PUT/PATCH) — mutate a field

```
PATCH {{BASE_URL}}/{{RESOURCE}}/{{CREATED_ID}}
Authorization: Bearer {{AUTH_TOKEN}}
Content-Type: application/json

{
  "{{FIELD_NAME}}": "{{UPDATED_VALUE}}"
}
```

**Assert:**
- Status: `200 OK`
- Body `{{FIELD_NAME}}` == `{{UPDATED_VALUE}}`.

---

### 4. Delete (DELETE)

```
DELETE {{BASE_URL}}/{{RESOURCE}}/{{CREATED_ID}}
Authorization: Bearer {{AUTH_TOKEN}}
```

**Assert:**
- Status: `204 No Content` (or `200 OK`).

Confirm deletion:

```
GET {{BASE_URL}}/{{RESOURCE}}/{{CREATED_ID}}
Authorization: Bearer {{AUTH_TOKEN}}
```

**Assert:**
- Status: `404 Not Found`.

---

### 5. Validation error — missing required field

```
POST {{BASE_URL}}/{{RESOURCE}}
Authorization: Bearer {{AUTH_TOKEN}}
Content-Type: application/json

{}
```

**Assert:**
- Status: `422 Unprocessable Entity` (or `400 Bad Request`).
- Response body includes a human-readable error referencing `{{FIELD_NAME}}`.
- No resource is created (follow-up GET to list endpoint returns same count).

---

### 6. Unauthorized — no token

```
GET {{BASE_URL}}/{{RESOURCE}}
```

**Assert:**
- Status: `401 Unauthorized` (or `403 Forbidden`).
- No data is returned.
