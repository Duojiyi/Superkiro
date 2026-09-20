# Superkiro

Desktop client, model gateway, service-card billing, and web administration.
The portal remains in `apps/portal-ui`; the redesigned admin is in `apps/admin-ui`.

## Local development

```sh
npm --prefix apps/desktop-ui ci
npm --prefix apps/desktop-ui run build
cargo run --locked -p desktop-host --bin Superkiro

# Web administration (a separate frontend)
npm --prefix apps/admin-ui ci
npm --prefix apps/admin-ui run dev
```

The admin development page is `http://localhost:3000/admin/`; Vite serves the UI
and proxies `/api` to the local gateway on port 19820. Browser authentication
requires a trusted HTTPS reverse proxy and a matching `ADMIN_ORIGIN`, plus
`ADMIN_BROWSER_LOGIN=true` and `ADMIN_PASSWORD_HASH`. Plain HTTP is only a UI
preview, not a supported administrator login setup.

The desktop runtime is Rust/Tauri 2 with a React frontend. On Windows,
`启动Debug客户端.bat` builds and starts this same native client. `run_desktop.py`
and `requirements-desktop.txt` remain for legacy bridge regression checks, not
the current desktop launch or packaging path.

Debug uses the same customer UI as release. Card validation does not activate Kiro;
connection changes require an explicit action and confirmation before closing Kiro.

## Native packages

```sh
python scripts/build_desktop.py
```

The Windows build needs Rust/MSVC, Node.js/npm and Python on the build machine;
end users need WebView2, not Python or Node.js.

GitHub Actions `Desktop Native Packages` builds Windows x64, macOS Apple Silicon and Intel
on their native runners. macOS bundles are archived with `tar` to preserve executable
permissions. Artifacts are **unsigned test builds**, not notarized commercial releases.
Apple Developer signing/notarization credentials and Windows signing credentials are
not included. Do not disable Gatekeeper or TLS validation to work around failures.

Mac uses Cocoa and left-side traffic-light controls. Installation detection supports
`/Applications/Kiro.app` and `~/Applications/Kiro.app`; a custom application may be
selected in settings. Native quit asks Kiro to close gracefully; timeout/cancel does
not force-kill or proceed with patching. Changes to an upstream signed app and a real
IDE request/restore cycle still require acceptance on a physical Mac.

CA trust is application-scoped; no system-wide root is installed. The included
`deploy/server-ca.pem` is a public certificate, not a private key. Native credential
storage uses Windows Credential Manager or macOS Keychain, never a plaintext fallback.
The gateway address stays out of ordinary customer views and sanitized reports.

## Service cards

| Credits | Display tier | Devices |
| ---: | --- | ---: |
| 1,000 | PRO | 1 |
| 2,000 | PRO+ | 1 |
| 5,000 | PRO Max | 1 |
| 10,000 | Power | 1 |

Display tiers are our service entitlements, not official Kiro subscriptions. Tier is
based on issued credits, not the remaining balance. See `crates/billing/TIER_POLICY.md`
for legacy cards, binding limits and deployment migration boundaries. Production
credentials, issued cards, logs and runtime data are excluded from this repository.

## Verification

```sh
python -m unittest -v test_desktop_bridge_regression test_desktop_runtime
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
```

Use isolated fixtures for frontend visual checks. Passing mock-based tests or package
builds does not certify real IDE interaction, production billing or notarization.

### Frontend acceptance

Desktop visual contracts: `python -B -m unittest -v test_superkiro_desktop_ui`
(requires Playwright and Chromium). Admin checks live in `apps/admin-ui/tests`:
`contracts.cjs`, `visual.cjs` (unauthenticated) and `visual-authenticated.cjs`
(populated fixture data). The CI frontend job builds and runs these checks.
Screenshots are local acceptance evidence, not production data.

Desktop daily usage is settled ledger data in UTC; token/model totals cover the
last 30 UTC dates and the chart displays the last seven. Missing archived detail
is reported as unavailable, never synthesized. Dollar estimates need an explicit
reference price and are not inferred from private upstream costs.
