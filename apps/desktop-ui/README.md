# Superkiro Desktop UI

React 19 + strict TypeScript + Vite frontend for the Rust Tauri 2 desktop host.

- `npm ci`
- `npm run build`: typecheck then write production assets to `dist/`.
- `npm test`: component and IPC boundary tests with mocked host responses.
- `python verify_browser.py`: requires Python Playwright and Chromium; serves `dist/` temporarily, injects a mock Tauri bridge, checks horizontal overflow and captures compact viewport states into ignored `verification/`. Server and browser close at completion. This is not native E2E or pixel acceptance.

IPC uses `invoke('api', {path, method, body})` and `invoke('native', {method, args})`, with no HTTP session token, Python bridge, or frontend credential persistence. The host owns all local operations. UI screen names request login 480×620 and main pages 620×820. Gateway addresses are never displayed or included in diagnostics.

Login does not activate. Configuration success does not imply verified model service. Timed-out mutations are not retried; UI blocks further mutations until a fresh client session. Memory data requires an installed, running Kiro and actual samples; no process means no curve or optimization success. Automatic maintenance reads the host memory_maintenance status; absent fields remain unknown, enabled does not mean a trim occurred, and raw maintenance errors are never rendered. No fabricated per-step connection progress or dollar pricing is shown.

Reports allowlist check names/levels, excluding arbitrary host details. Export uses browser download and only says the download was requested, not that a file was saved. Native save-dialog/download behavior, tray lifecycle, real credentials, real Kiro activation/recovery and model requests require separate Windows-host acceptance. Window close follows the actual `tray_available` capability, otherwise confirms restoration before exit.

Tray-enabled hosts hide on native close. Explicit exit restores first, then calls native exit. The desktop-exit-request Tauri event opens the same restoration confirmation; listener cleanup handles component lifecycle.



## Visual verification boundaries and remaining differences

Browser mock checks login **including its error state at 480×620**, other pages at 620×820, transparent root backgrounds, shared silver vertical-gradient primary buttons, and centered single-peak login bars. Screenshots are saved with transparent corners. These checks do not prove native window transparency, tray behavior, animation fidelity, or pixel equivalence. Windows host transparency must be checked with the newly built executable, not an older opaque build.

Remaining differences against `docs/design-superkiro-desktop` (not claimed 1:1):
- Login typography, spacing, bar brightness and stripe details have not been individually calibrated. The added recovery entry and secure-storage wording intentionally change the footer arrangement.
- Login busy animation is an opacity pulse, not a scanning highlight.
- Overview action positions/order differ; pending overview uses a wave instead of the original flat bars.
- Usage chart proportions differ, including an overly wide single column; navigation uses Unicode icons rather than the original line icons.
- Installation-missing, connecting, expired and restore-failed layouts are simplified. Reports, dialogs and individual component radii are not pixel matched.
- No ring gauge is implemented; no matching ring-gauge reference was found in the 14 inspected design PNGs.

Do not infer actual memory optimization without Kiro/process measurements, or native rounded-corner acceptance from CSS screenshots.
