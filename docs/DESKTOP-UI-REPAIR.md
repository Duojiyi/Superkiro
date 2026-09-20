# Desktop Debug UI Repair

## Scope

The desktop UI now uses HTML controls, responsive CSS and external JavaScript instead of baked-in screenshot labels and transparent click targets. No backend feature is considered implemented merely because it has a UI.

Fixed: overlapping inputs, non-responsive fixed canvases, hidden scrolling, fake status/credit claims, invisible navigation, global Enter activation, short-lived error toasts, missing confirmations, duplicate submission, refresh losing bridge authentication, and diagnostics checking an unrelated localhost gateway. Backend diagnostics now require a validated explicit gateway. Empty activation cards are rejected before CLI invocation. Static pages have a same-origin Content Security Policy.

## Debug Build

Run `启动Debug客户端.bat`. This executes `cargo build --locked -p patch-engine --bin patch-cli`, then starts the desktop bridge. The existing project uses a Python/Chrome desktop shell with the compiled `target/debug/patch-cli.exe`; there is no separate Rust GUI executable. No release build is substituted.

## Verification

- Debug build: passed, dev profile with debug info.
- `python -m unittest test_desktop_bridge_regression test_desktop_ui`: 8 tests passed.
- Browser regression covers all four views at widths 320/441/680/1280, no horizontal overflow; token removal from URL and preservation on reload; card visibility; confirmation cancellation; failed/successful activation; unbind; diagnostic gateway selection and safe rendering; card cleared after success/reload; no JavaScript errors.
- Browser workflows mock the CLI and do not consume real cards, modify Kiro or prove real cloud acceptance.
- Actual debug CLI status was read: Kiro 1.1.14 Running, authenticated false, no active snapshot.
- Live IDE workflow, private-CA trust and cloud features still require separate acceptance. Do not bypass certificate validation.
