# Superkiro Web Admin implementation handoff

Scope: `apps/admin-ui/**` only. No backend, desktop, runner, production calls, repository push, node_modules edits, or dist edits. Production build verification writes to `.build-check/`.

## Backend contract

Card creation keeps the entitlement group independent of the issuance tier. Groups come from the existing commercial-config endpoint. Creation is disabled until a real group has been loaded.

| Label | Points | templateId |
| --- | ---: | --- |
| PRO | 1,000 | tier-1000 |
| PRO+ | 2,000 | tier-2000 |
| PRO Max | 5,000 | tier-5000 |
| Power | 10,000 | tier-10000 |

POST `/api/v1/admin/cards/batch` sends `{count, groupId, templateId, maxDevices: 1}`. It does not send `creditTotal`. Default template is explicitly `tier-2000`; enforcement remains server-side. No new endpoints were introduced.

## Implementation

- White sidebar and header, neutral workspace, rust-red actions, pale emphasis panels, nine Chinese navigation labels, Superkiro document title and branding.
- Live overview, searchable card assets, real provider/Key selection, independent discovery results and permission drafts, structured existing group/model fields, fixed-price version drafts with exact micro-credit conversion, traces with selection/details, financial exports, announcement preview/confirmation, configuration audit and retained security forms.
- Publishing retains revision checks, reasons, immutable price history and confirmation. Advanced JSON remains available for initial entries, unsupported pricing modes, and full configuration coverage.
- Single-card/single-device issuance wording; no editable multi-device issuance setting. Existing cards display the actual server-returned device limit rather than pretending a migration has occurred.
- Unavailable data is empty/unconfigured. No approved-design numbers or rows were copied into runtime data. Overview chart is explicitly a histogram of the latest retrieved traces, not a complete daily chart. Financial ledger costs are distinguished from verified procurement costs and actual cash revenue.

## Verification

Run from this directory:

```powershell
npm run build -- --outDir .build-check
node tests/contracts.cjs
$env:PLAYWRIGHT_MODULE='C:/Users/Administrator/.cache/codex-runtimes/codex-primary-runtime/dependencies/node/node_modules/playwright'
$env:CHROME_PATH='C:/Users/Administrator/.cache/puppeteer/chrome/win64-150.0.7871.24/chrome-win64/chrome.exe'
node tests/visual.cjs
```

The visual script starts an ephemeral loopback-only static server and closes it and the browser when finished. API requests return 401 locally; external requests are blocked. Screenshots show actual unauthenticated UI, not fixture business data. Checks cover nine tabs, desktop/mobile overflow, password form, four tier choices, malformed advanced JSON, and runtime exceptions. Contract checks cover price conversion/invalid values, in-memory credentials, four issuance payloads, and revision publishing. These checks do not claim authenticated end-to-end or production acceptance.

## Screenshot comparison

Approved images are in `../../docs/design-v4-superkiro/`. Actual screenshots are in `visual-check/`.

| Approved | Actual local screenshot |
| --- | --- |
| d4W5G.png | overview-desktop.png |
| cvHzH.png | cards-desktop.png |
| zj1uX.png | providers-desktop.png |
| v5y25f.png | pricing-desktop.png |
| qVSnv.png | groups-desktop.png |
| XzjIz.png | trace-desktop.png |
| AV4pT.png | finance-desktop.png |
| AlMT1.png | security-desktop.png |
| rJhuJ.png | announcements-desktop.png |

Additional checks: `card-creation-desktop.png`, `authentication-desktop.png`, `overview-mobile.png`, `cards-mobile.png`.

The shell, color treatment, hierarchy and panel layouts follow the approved references. This is not a pixel-identical populated-data comparison: screenshots intentionally have no business rows; unsupported daily aggregation, all-operation auditing, TOTP enrollment, targeted announcement distribution and procurement setup are not fabricated. Full legacy configuration coverage remains below the simplified panels.

## Changed source and test files

- index.html
- src/App.tsx
- src/api.ts
- src/CommercialEditor.tsx
- src/ProviderKeyEditor.tsx
- src/index.css
- src/pricing.ts (new)
- tests/contracts.cjs (new)
- tests/visual.cjs (new)
- IMPLEMENTATION.md (this handoff)

Generated verification outputs: `.build-check/` and `visual-check/`. Do not treat these as production deployment artifacts.


## Authenticated fixture verification (follow-up)

`node tests/visual-authenticated.cjs` uses `tests/fixture-api.cjs` behind an ephemeral loopback HTTP server, requires login through the existing password form, and checks bearer authorization. No fixture is imported by application code. External browser requests are blocked; unhandled mock endpoints fail the test. Screenshots have a LOCAL FIXTURE watermark and use deliberately test-labelled rows and invalid card credentials.

- All nine pages have populated API data and desktop/mobile screenshots under `visual-check/authenticated/`.
- `visual-check/comparison.html` pairs all nine approved references with authenticated screenshots; mobile links are included.
- Overview restores successful request count and success rate. Count uses `status=success`; rate divides by success + error + client_aborted, excluding in_progress and unknown statuses. Empty denominator shows an em dash. This is explicitly the latest trace sample, not all-day reporting. Fixture asserts 18 successes / 23 completed = 78.3%.
- Chart bins all 24 returned requests into 12 local two-hour slots, potentially combining days, explicitly labelled. Bar heights scale to the maximum bin; tooltips give counts. Date selection is explicitly unsupported/disabled.
- Tests exercise card search, freeze/unfreeze, all four issuance templates with single-device payload, one-time generated credentials, CSV download, trace filtering/detail, financial CSV export, and populated pricing template inputs.
- All nine pages checked at 1440x1080 and 390x844. Mobile tables scroll internally; provider action columns are verified reachable by horizontal scrolling. No document horizontal overflow or uncaught runtime errors.
- Machine-readable checks and test mutation payloads: `visual-check/authenticated/results.json`.

Remaining reference differences are intentional capability boundaries, not claimed pixel parity: no daily aggregate date filter, no server-side draft count, no verified cash revenue/procurement setup, no full-operation audit or TOTP enrollment, no targeted announcement delivery, no standalone connectivity test or browser KEK rotation. Existing API-backed forms and advanced configuration remain available. Fixture acceptance does not replace integration testing against a real authenticated backend.
