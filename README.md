# Superkiro

Desktop client, model gateway, service-card billing, and web administration.
The portal remains in `apps/portal-ui`; the redesigned admin is in `apps/admin-ui`.

## Local development

```sh
python -m pip install -r requirements-desktop.txt
cargo build --locked -p patch-engine --bin patch-cli
python run_desktop.py --dev
cd apps/admin-ui
npm ci
npm run build
```

Debug uses the same customer UI as release. Card validation does not activate Kiro;
connection changes require an explicit action and confirmation before closing Kiro.

## Native packages

```sh
python -m pip install pyinstaller==6.16.0
python scripts/build_desktop.py
```

GitHub Actions `Desktop Packages` builds Windows x64, macOS Apple Silicon and Intel
on their native runners. macOS bundles are archived with `ditto` to preserve executable
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
