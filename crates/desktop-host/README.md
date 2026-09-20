# Native Desktop Host

Windows Tauri 2 host. Frontend assets are embedded from `../../apps/desktop-ui/dist`.
The production executable directly links patch-engine; it does not run Python,
Node, patch-cli, or a localhost HTTP bridge. WebView2 Runtime is required on Windows.
The release build uses the static MSVC runtime and produces `dist/Superkiro.exe`.
Launch that EXE directly. The repository-root legacy desktop/debug `.bat` files
still invoke the Python shell and are NOT launchers for this native application.

## Build and Test

From the repository root, run `python scripts/build_desktop.py` (build-time Python
only), then `cargo test --locked -p desktop-host`. Rust/MSVC and Node/npm are build
prerequisites. For manual builds, run `npm ci` and `npm run build` in
`apps/desktop-ui` before `cargo build --locked -p desktop-host`.
`cargo tauri build` runs the configured frontend build hook, but is not required.

## Frontend Contract

Use `invoke` from `@tauri-apps/api/core`. Commands reject with a string on error.
No loopback session token is used: commands require the main local Tauri webview.
No remote-origin capabilities, filesystem or shell plugins are enabled.

- `invoke('api', {path, method, body})`: body is `{}` for requests without data.
- GET `/api/status`: process_state, kiro_installed, kiro_install_path, kiro_version,
  has_snapshot, recovery_pending, authenticated, gateway_url, suggested_gateway_url, portal_url,
  platform (`win32`/`darwin`/`linux`), model_service_available (`null`, unverified),
  tray_available (`true`), app_version, memory_maintenance.
- GET `/api/usage`: `{success:true, settledUsage:object|null, usage:object}`.
- GET `/api/doctor?gateway_url=...`: diagnostic status plus item name/level only; raw detail is omitted.
- GET `/api/memory/sample`: patch-engine MemorySnapshot unchanged, errors reject.
- POST `/api/verify-card`: body `{gateway_url,card_key}`; returns
  `{success:true,authorization,gateway_url}`. Read-only portal lookup.
- POST `/api/activate`: body `{gateway_url,card_key,close_kiro_confirmed}`;
  calls patch-engine activate_and_launch, returns `{success:true}`.
- POST `/api/restore`, `/api/launch`: body `{}`, returns `{success:true}`.
- POST `/api/unbind`: body `{card_key}`, returns `{success:true}`.
- POST `/api/memory/trim`: real TrimResult; unsupported OS/no Kiro processes reject.
- POST `/api/heartbeat`: `{status:'alive'}` (compatibility, no HTTP watchdog).
- `invoke('native', {method,args})`: minimize/maximize/close/drag/screen return true;
  screen accepts `[name]`: connect/login use 480x620, other pages use 620x820.
- get_close_behavior `[]`: returns `"tray"` (default), `"minimize"`, or `"exit"`.
- set_close_behavior `[value]`: persists one of those exact strings and returns true;
  invalid input or a failed save rejects without changing the effective preference.
  Stored in app_config_dir/window-close-preference, independently of installation
  settings. Missing/unreadable/invalid saved values safely default to tray.
- close `[]` remains an explicit hide-to-tray command regardless of preference.
  Custom header controls should read get_close_behavior and dispatch close,
  minimize, or the existing frontend exit confirmation accordingly.
- open_external `[url]`: only configured portal and Kiro download page, returns true.
- pick_install_path `[]`: null on cancellation, otherwise `{success:true,path}`;
  validates Kiro and refuses changes while a takeover snapshot exists.
- get_remembered_card `[]`: string|null; set_remembered_card `[card]` and
  clear_remembered_card `[]`: true. Native OS credential store only, errors reject.

Operations are serialized. Timeouts/disconnects must not be treated as cancellation
or retried automatically: query status/doctor before repeating a mutation.
TLS CA configuration is validated before engine HTTP operations. Unexpected engine
panics become command errors, not silent loss of the command response.

## Tray and Maintenance

The native OS close callback always prevents direct destruction and applies the
saved close behavior: tray hides the window, minimize minimizes it, and exit shows
and focuses the window then emits `desktop-exit-request` (null payload) to the
main webview. Exit preference never bypasses the frontend safe-restore confirmation,
even with no recovery snapshot. Cancellation leaves the client running. The existing
frontend listener should confirm restoration then call native exit; its operation
and recovery guards remain unchanged. No new event listener is required.

Explicit native close still hides in the real system tray; Show restores it. Launching the executable again also shows and focuses the existing window through the Tauri single-instance plugin; it must not start a second maintenance task. Tray Exit
exits only when no operation or takeover snapshot remains. Otherwise it shows
the window and emits `desktop-exit-request`; the frontend must confirm restoration
and call `native('exit', [])` after restoring. That command rejects while an
operation/snapshot remains. Closing does not silently restore or terminate Kiro.

`memory_maintenance` reports enabled, mode (`automatic` on Windows, `monitor-only`
elsewhere), threshold_mb (2500), cooldown_seconds (300), last_sample_mb, last_error,
and last_trim. Sampling runs every 60 seconds; only above-threshold Windows
samples with processes can trigger working-set trimming, no more than once in
five minutes. Manual attempts also restart the automatic cooldown; explicit manual
attempts are not rate-limited. The first sample occurs immediately after startup.
An attempt with zero successful trims and any failures returns an error.
No process termination or cache deletion is performed by automatic maintenance.
The loop runs only while Superkiro is running (including in the tray).

Limitations: the current scope is name-based Kiro roots and their descendants,
not a verified selected installation or user session. PID reuse between sampling
and opening a process is not protected by process-creation identity checks.
Working-set reduction is not heap release and may cause page faults on reuse;
the before/after delta is observational, not proven savings caused by trimming.
There is no system-pressure or idle detection, so the fixed threshold is not a
claim of optimal performance. These require further hardening and native workload
benchmarks before stronger safety/performance claims.

Recovery gating uses snapshot OR the desktop session rollback file, independent
of authenticated/token readability. An unreadable session path fails closed.
Status remains available if credentials cannot be read (authenticated=false).
Authorization, usage and doctor responses are allowlisted; raw gateway extension
fields, tokens and diagnostic detail never enter these frontend responses.

## Native Visual Acceptance Limits

Windows uses `tauri.windows.conf.json` to enable a transparent, undecorated window
with an explicitly transparent background and native shadow disabled. Setup repeats
these Windows-only settings for builds which use only the base Tauri config.
Tauri's undecorated Windows shadow adds a white 1px native border; disabling it
removes that border outside the rounded web content (and the native shadow).
The frontend must keep html/body/root backgrounds transparent outside its rounded
shell. This platform override does not enable macOS private APIs or transparency.
Native corner transparency, tray interaction and close-to-hide behavior must be
verified using the final EXE; browser screenshots alone are not native acceptance.
