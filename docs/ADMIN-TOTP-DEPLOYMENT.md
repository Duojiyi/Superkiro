# Administrator TOTP and browser sessions

## Configuration

Browser login requires `ADMIN_BROWSER_LOGIN=true`, an exact HTTPS `ADMIN_ORIGIN`
(no trailing slash), and `ADMIN_PASSWORD_HASH` (bcrypt cost 4 through 16).
Set `ADMIN_TOTP_SECRET` to enable mandatory TOTP for this single administrator.
When it is absent, password-only login remains available and both 2FA status flags
are false. An empty, malformed or non-Unicode value fails closed: administrator
browser login is unavailable, never silently downgraded to password-only.

The secret must be canonical uppercase RFC 4648 Base32 without padding or spaces,
decoding to 20 through 64 bytes. Generate at least 20 cryptographically random
bytes with trusted offline provisioning tooling; provision the same secret into
the authenticator using SHA1, six digits and a 30-second period. No real secret
or enrollment URI belongs in source control, screenshots, support tickets or logs.
Inject the variable using the deployment secret manager or an owner-only runtime
environment file. Do not put it in CLI arguments or frontend build variables.
This implementation never returns the secret or exposes enrollment/rotation APIs.

Restart the gateway after changing configuration. Existing in-memory sessions do
not survive restart. Verify the anonymous session response reports the intended
state, then perform a successful login with an authenticator before considering
deployment complete. The security UI must display **not configured** when false;
no simulated enrollment, recovery codes or enable/disable buttons are supported.
For recovery, an operator must securely rotate the secret and restart. Removing
the variable explicitly disables 2FA and must be treated as a security change.

Once the secret is provisioned and a TOTP login has succeeded, also set
`ADMIN_TOTP_REQUIRED=true`. The gateway then refuses to start without
`ADMIN_TOTP_SECRET`, so a later deployment that lost the secret cannot silently
fall back to password-only login. Without TOTP, the gateway logs a reminder at
startup.

## Login throttling and the operator's lane

Login budgets are ten attempts a minute per source: an IPv4 address, or an IPv6
/64. At most 4096 sources are tracked, and at most four password checks run at
once. A source that logged in successfully within the last 30 days keeps its own
budget table and one reserved password check, so strangers who fill the table or
keep every check busy cannot lock the operator out. Refusals caused by a full
table or busy checks are logged at most once a minute.

## Time, throttling and replay protection

TOTP accepts the previous, current and next 30-second step (clock skew +/- one
step). Maintain synchronized wall clocks. Verification compares all three
candidates using ring's constant-time comparison. A mutex protects a monotonic
accepted-step watermark: only a step newer than every previously accepted step
can succeed, including across concurrent logins. Older steps are rejected even
if they are still within the time window. A valid code is consumed once password
verification succeeds, even if session issuance subsequently fails.

On startup, all steps through startup_step + 1 are quarantined. This prevents a
restart from immediately reaccepting codes consumed by the preceding process.
With synchronized authenticator/server clocks, wait until the second 30-second
boundary after startup (approximately 30 to 60 seconds) before logging in. This
assumes wall time does not move backwards across process restarts.

Replay and rate-limit state are shared by clones of one BrowserAuth instance,
not persisted or shared between independent gateway processes. **Deploy a single
admin-serving process.** Multi-replica deployments require a shared atomic replay
store and rate limiter before they can claim equivalent protection. Do not use
rolling overlap of admin-serving processes with the same secret.

All login requests, including failed passwords, missing/malformed codes and
replays, share a bounded budget of ten attempts per 60-second window. Exhaustion
returns HTTP 429 with `Retry-After: 60`. This is deliberately global for the single
administrator; spoofing client IP addresses cannot bypass it.

Backup automation uses the same POST `totpCode` field and algorithm. Store its
provisioning credential only in the owner-only credentials file maintained by the
backup tooling; never log that file or generated codes. Backup and interactive
login cannot both consume the same time step. On a generic 401 caused by a code
already used, wait for the next time boundary and generate a new code. Never
retry a cached code in a tight loop; honor 429 backoff. A client whose clock is
behind a previously accepted future step may need to wait an additional step.

## Exact frontend contract

All these responses use `Cache-Control: no-store`. Browser requests use the
host-only `__Host-admin_session` cookie, never localStorage bearer credentials.
Mutation requests must have `Origin` exactly matching `ADMIN_ORIGIN`.

### POST /api/v1/admin/session

JSON request: `{ "username": "admin", "password": "...", "totpCode": "..." }`.
`totpCode` is an optional string when not configured, mandatory when configured;
it must contain exactly six ASCII decimal digits, preserving leading zeros.
Successful HTTP 200 JSON:

```json
{
  "success": true,
  "authenticated": true,
  "expiresAt": 1800001800,
  "expiresIn": 1800,
  "twoFactorEnabled": true,
  "totpRequired": true
}
```

`expiresAt` is an absolute Unix timestamp in **seconds**, not milliseconds, and
`expiresIn` the remaining whole seconds. A session ends after 30 minutes without
use and 8 hours after sign-in at the latest: each authenticated request moves
the deadline on, and every authenticated response reports it in the
`x-admin-session-expires` header. Requests sent with `x-admin-background: 1`
(the console refreshing itself) do not count as use. The Secure, HttpOnly,
SameSite=Strict cookie lasts 8 hours; the server enforces the idle limit.
Re-login revokes the previous supplied browser session. No access token or CSRF
token is returned in the login body; obtain CSRF from authenticated GET below.

Wrong username/password, absent/invalid TOTP and replay all return the same HTTP
401 error envelope. Malformed JSON is 400, wrong content type is 415, rejected
origin is 403 and exhausted budget/session capacity is 429. Do not infer which
credential was wrong from the error text. Rate-limit exhaustion includes the
Retry-After header; session-capacity exhaustion does not.

### GET /api/v1/admin/session

Authenticated HTTP 200 includes all POST success fields plus `role: "admin"`
and `csrfToken: string`. `expiresAt` is read from the already verified session
claims and stays unchanged across GETs; `expiresIn` decreases with elapsed time.
Use `expiresAt * 1000` for a JavaScript deadline and/or remaining seconds to avoid
clock-skew problems. Revalidate on focus/visibility and clear sensitive UI state
on expiry, logout or HTTP 401. GET does not renew the cookie or session.

Anonymous, expired or revoked cookie requests return HTTP 401:

```json
{
  "success": false,
  "error": "Invalid administrator credentials or verification code",
  "authenticated": false,
  "twoFactorEnabled": true,
  "totpRequired": true
}
```

The two booleans reveal only whether deployment configured mandatory TOTP; they
are identical and contain no secret or account enumeration information. Expired
or revoked parsed session cookies are cleared with Max-Age=0. No CSRF token or
expiration is disclosed to anonymous callers. When browser configuration itself
is invalid, the outer router fails closed with its existing generic 401; flags
may be absent and clients must treat that state as unknown/unavailable, not as
proof that 2FA is disabled.

Authenticated mutations also require `X-CSRF-Token`. Existing
`POST /api/v1/admin/session/revoke` behavior is unchanged.

## Local verification

Run without production connectivity:

```text
cargo test -p gateway --lib facade::admin_login::tests --offline
cargo test -p gateway --test admin_browser_login_test --offline
```

Tests use public RFC 6238 fixtures only. They cover RFC vectors, drift windows,
format errors, replay/concurrency, restart quarantine, failed configuration,
mandatory codes, throttling, cookie/CSRF/revocation and actual expiry metadata.
