#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod errors;
mod update;
mod window_preferences;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{
    Emitter, LogicalSize, Manager, PhysicalPosition, PhysicalRect, PhysicalSize, WebviewWindow,
};
use window_preferences::{ClosePreference, WindowPreferences};

#[tauri::command]
async fn api(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<backend::Host>>,
    path: String,
    method: String,
    body: Value,
) -> Result<Value, Value> {
    local_window(&window).map_err(|e| errors::classify(&e, &path, &method))?;
    let host = Arc::clone(state.inner());
    if method == "GET" && path.split('?').next() == Some("/api/operation") {
        return host
            .operation_status()
            .map_err(|e| errors::classify(&e, &path, &method));
    }
    if (method == "GET"
        && matches!(
            path.split('?').next(),
            Some("/api/status" | "/api/announcements")
        ))
        || (method == "POST" && path.split('?').next() == Some("/api/heartbeat"))
    {
        return backend::dispatch(&host, &path, &method, body)
            .await
            .map_err(|e| errors::classify(&e, &path, &method));
    }
    if backend::reads_only(&method, &path) {
        let (worker_path, worker_method) = (path.clone(), method.clone());
        return tauri::async_runtime::spawn_blocking(move || {
            tauri::async_runtime::block_on(backend::dispatch(
                &host,
                &worker_path,
                &worker_method,
                body,
            ))
        })
        .await
        .unwrap_or_else(|_| Err("Desktop engine failed unexpectedly".into()))
        .map_err(|e| errors::classify(&e, &path, &method));
    }
    tauri::async_runtime::spawn(async move {
        // The detached task retains its lock despite a webview timeout.
        let tracked = backend::is_tracked_operation(&method, &path);
        let worker_host = Arc::clone(&host);
        let worker_path = path.clone();
        let operation_method = method.clone();
        backend::run_operation(&host, &path, tracked, &operation_method, async move {
            // Blocking helpers cannot starve the operation status endpoint.
            tauri::async_runtime::spawn_blocking(move || {
                tauri::async_runtime::block_on(backend::dispatch(
                    &worker_host,
                    &worker_path,
                    &method,
                    body,
                ))
            })
            .await
            .unwrap_or_else(|_| Err("Desktop engine failed; inspect recovery state".into()))
        })
        .await
    })
    .await
    .map_err(|_| errors::classify("Desktop engine failed unexpectedly", "", "POST"))?
}

fn local_window(window: &WebviewWindow) -> Result<(), String> {
    let url = window.url().map_err(|e| e.to_string())?;
    if window.label() != "main"
        || !(url.scheme() == "tauri" && url.host_str() == Some("localhost")
            || matches!(url.scheme(), "http" | "https")
                && url.host_str() == Some("tauri.localhost"))
    {
        return Err("Unauthorized webview".into());
    }
    Ok(())
}

struct ScreenBounds {
    size: PhysicalSize<u32>,
    min_size: PhysicalSize<u32>,
    position: PhysicalPosition<i32>,
}

fn screen_bounds(
    requested: LogicalSize<f64>,
    scale: f64,
    work: PhysicalRect<i32, u32>,
    position: PhysicalPosition<i32>,
    frame: PhysicalSize<u32>,
) -> Result<ScreenBounds, String> {
    if !scale.is_finite()
        || scale <= 0.0
        || work.size.width <= frame.width
        || work.size.height <= frame.height
    {
        return Err("Invalid monitor work area or scale factor".into());
    }
    let available = PhysicalSize::new(
        work.size.width - frame.width,
        work.size.height - frame.height,
    );
    let requested = requested.to_physical::<u32>(scale);
    let minimum = LogicalSize::new(480.0, 540.0).to_physical::<u32>(scale);
    let size = PhysicalSize::new(
        requested.width.min(available.width),
        requested.height.min(available.height),
    );
    // Keep desktop coordinates physical: monitors can have different DPI and negative origins.
    let clamp_axis = |value: i32, origin: i32, extent: u32, outer: u32| {
        i64::from(value).clamp(
            i64::from(origin),
            i64::from(origin) + i64::from(extent) - i64::from(outer),
        ) as i32
    };
    Ok(ScreenBounds {
        size,
        min_size: PhysicalSize::new(
            minimum.width.min(available.width),
            minimum.height.min(available.height),
        ),
        position: PhysicalPosition::new(
            clamp_axis(
                position.x,
                work.position.x,
                work.size.width,
                size.width + frame.width,
            ),
            clamp_axis(
                position.y,
                work.position.y,
                work.size.height,
                size.height + frame.height,
            ),
        ),
    })
}

fn resize_screen(window: &WebviewWindow, page: &str) -> Result<(), String> {
    // Page changes must not restore or resize a user-maximized window.
    if window.is_maximized().map_err(|e| e.to_string())? {
        return Ok(());
    }
    let Some(monitor) = window.current_monitor().map_err(|e| e.to_string())? else {
        // Without a current work area, leave the user's geometry untouched.
        return Ok(());
    };
    let position = window.outer_position().map_err(|e| e.to_string())?;
    let outer = window.outer_size().map_err(|e| e.to_string())?;
    let inner = window.inner_size().map_err(|e| e.to_string())?;
    let requested = if matches!(page, "connect" | "login") {
        LogicalSize::new(480.0, 540.0)
    } else {
        LogicalSize::new(620.0, 820.0)
    };
    let bounds = screen_bounds(
        requested,
        monitor.scale_factor(),
        *monitor.work_area(),
        position,
        PhysicalSize::new(
            outer.width.saturating_sub(inner.width),
            outer.height.saturating_sub(inner.height),
        ),
    )?;
    // Lower the minimum first so small work areas can actually contain the window.
    window
        .set_min_size(Some(bounds.min_size))
        .map_err(|e| e.to_string())?;
    window.set_size(bounds.size).map_err(|e| e.to_string())?;
    if bounds.position != position {
        window
            .set_position(bounds.position)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
async fn native(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<backend::Host>>,
    method: String,
    args: Vec<Value>,
) -> Result<Value, Value> {
    let operation = method.clone();
    native_inner(window, state, method, args)
        .await
        .map_err(|e| errors::classify(&e, "native", &operation))
}

async fn native_inner(
    window: WebviewWindow,
    state: tauri::State<'_, Arc<backend::Host>>,
    method: String,
    args: Vec<Value>,
) -> Result<Value, String> {
    local_window(&window)?;
    let arg = args.first().and_then(Value::as_str).unwrap_or("");
    match method.as_str() {
        "minimize" => window.minimize().map_err(|e| e.to_string())?,
        "maximize" => if window.is_maximized().map_err(|e| e.to_string())? {
            window.unmaximize()
        } else {
            window.maximize()
        }
        .map_err(|e| e.to_string())?,
        "drag" => window.start_dragging().map_err(|e| e.to_string())?,
        "screen" => resize_screen(&window, arg)?,
        "get_close_behavior" => {
            return Ok(json!(window
                .app_handle()
                .state::<WindowPreferences>()
                .get()?
                .as_str()));
        }
        "set_close_behavior" => {
            window.app_handle().state::<WindowPreferences>().set(arg)?;
        }
        "close" => window.hide().map_err(|e| e.to_string())?,
        "exit" => {
            let _guard = state
                .operation
                .try_lock()
                .map_err(|_| "Operation in progress; wait before exiting")?;
            if state.recovery_pending() {
                return Err("Restore Kiro before exiting".into());
            }
            window.app_handle().exit(0);
        }
        "open_external" => {
            if !backend::external_allowed(arg)? {
                return Err("External URL not allowed".into());
            }
            open::that(arg).map_err(|_| "Cannot open browser")?;
        }
        "get_remembered_card" => {
            return match credential()?.get_password() {
                Ok(card) => Ok(json!(card)),
                Err(keyring::Error::NoEntry) => Ok(Value::Null),
                Err(_) => Err("Credential store unavailable".into()),
            }
        }
        "set_remembered_card" => {
            backend::validate_card(arg)?;
            credential()?
                .set_password(arg.trim())
                .map_err(|_| "Cannot save credential")?;
        }
        "clear_remembered_card" => match credential()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => {}
            Err(_) => return Err("Cannot clear credential".into()),
        },
        "update_check" => {
            let origin = update::origin(backend::configured_gateway())?;
            let client = backend::gateway_client(&origin)?;
            return update::check(&client, &origin, &state.update_state()).await;
        }
        "update_install" => {
            let origin = update::origin(backend::configured_gateway())?;
            let state_file = state.update_state();
            let release =
                update::available(&backend::gateway_client(&origin)?, &origin, &state_file)
                    .await?
                    .ok_or("[update:verify] No update is available")?;
            let mut reported = 0;
            let bytes = update::download(
                &backend::download_client(&origin)?,
                &origin,
                &release,
                |received, total| {
                    // A percent at a time is plenty for a progress bar.
                    let percent = received * 100 / total.max(1);
                    if percent > reported {
                        reported = percent;
                        let _ = window.emit(
                            "update-progress",
                            json!({"received": received, "total": total}),
                        );
                    }
                },
            )
            .await?;
            // Never in the middle of a takeover or a restore; the page tries again later.
            // The guard is held until the process exits, so a queued mutation cannot start
            // and be killed mid-write.
            let guard = state
                .operation
                .try_lock()
                .map_err(|_| "Operation in progress")?;
            tauri::async_runtime::spawn_blocking(move || {
                update::install(&state_file, &bytes, &release)
            })
            .await
            .map_err(|_| "[update:relaunch] The update stopped unexpectedly".to_string())??;
            // The new version is up and waiting for this process to end before it takes the
            // lock; it puts the old version back if it never gets there.
            window.app_handle().exit(0);
            drop(guard);
        }
        "update_confirm" => {
            // The window is up and talking to the host: the new version has proven itself.
            let state_file = state.update_state();
            tauri::async_runtime::spawn_blocking(move || update::confirm(&state_file));
        }
        "pick_install_path" => {
            let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await else {
                return Ok(Value::Null);
            };
            let _guard = state
                .operation
                .try_lock()
                .map_err(|_| "Operation in progress")?;
            return state.set_install_path(folder.path());
        }
        _ => return Err("Unknown native command".into()),
    }
    Ok(json!(true))
}

fn credential() -> Result<keyring::Entry, String> {
    if !cfg!(any(windows, target_os = "macos")) {
        return Err("Native credential store unavailable".into());
    }
    keyring::Entry::new("Superkiro", "card").map_err(|_| "Credential store unavailable".into())
}

fn show_main(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

// OS close requests honor the preference; native("close") explicitly hides to tray.
fn close_main(app: &tauri::AppHandle) -> Result<(), String> {
    let window = app
        .get_webview_window("main")
        .ok_or("Main window unavailable")?;
    match app.state::<WindowPreferences>().get()? {
        ClosePreference::Tray => window.hide().map_err(|e| e.to_string()),
        ClosePreference::Minimize => window.minimize().map_err(|e| e.to_string()),
        ClosePreference::Exit => {
            // Always go through the frontend confirmation, even without a snapshot.
            // Only native("exit") may perform the final guarded exit.
            show_main(app);
            window
                .emit("desktop-exit-request", ())
                .map_err(|e| e.to_string())
        }
    }
}

fn main() {
    // Before the single-instance plugin or the app lock: an update's first boot waits for
    // the version it replaced to exit, and a version that never confirmed rolls back and
    // hands off to the old one, which this process then steps aside for.
    if let update::Startup::Exit = update::startup(update::state_path().as_deref()) {
        return;
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            show_main(app);
        }))
        .setup(|app| {
            #[cfg(debug_assertions)]
            if let Some(window) = app.get_webview_window("main") {
                window.set_title("Superkiro Debug")?;
            }
            #[cfg(windows)]
            if let Some(window) = app.get_webview_window("main") {
                // Tauri's undecorated Windows shadow adds a white 1px native border.
                // CSS border-radius cannot clip that border outside the webview.
                window.set_shadow(false)?;
                window.set_background_color(Some(tauri::window::Color(0, 0, 0, 0)))?;
            }
            let config = app.path().app_config_dir()?;
            // The lock, the install path and the operation record describe this machine,
            // not the user: in roaming AppData, a profile redirected to a share made the
            // lock refuse the client on a second PC.
            let local = app.path().app_local_data_dir()?;
            backend::adopt_roaming_state(&config, &local);
            let host = match backend::Host::new(local) {
                Ok(host) => Arc::new(host),
                Err(error) => {
                    // Setup failing leaves no window, so without this the client simply
                    // never appears. The single-instance hand-off covers only this
                    // Windows session; one open in another session ends up here.
                    let running = error
                        .downcast_ref::<patch_engine::SingleInstanceError>()
                        .is_some_and(|e| matches!(e, patch_engine::SingleInstanceError::AlreadyRunning(_)));
                    let message = if running {
                        "Superkiro 已经在运行，可能是在你的另一个 Windows 会话中。请先关闭那个 Superkiro，再重新打开。".to_string()
                    } else {
                        format!("Superkiro 无法启动：{error}")
                    };
                    rfd::MessageDialog::new()
                        .set_level(rfd::MessageLevel::Warning)
                        .set_title("Superkiro")
                        .set_description(message)
                        .show();
                    return Err(error);
                }
            };
            app.manage(WindowPreferences::load(config));
            app.manage(Arc::clone(&host));
            let show = MenuItem::with_id(app, "show", "显示 Superkiro", true, None::<&str>)?;
            let exit = MenuItem::with_id(app, "exit", "退出 Superkiro", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &exit])?;
            TrayIconBuilder::new()
                .icon(
                    app.default_window_icon()
                        .ok_or("Missing tray icon")?
                        .clone(),
                )
                .tooltip("Superkiro")
                .menu(&menu)
                .on_menu_event(|app, event| {
                    if event.id.as_ref() == "show" {
                        show_main(app);
                    }
                    if event.id.as_ref() == "exit" {
                        let host = app.state::<Arc<backend::Host>>();
                        if let Ok(_guard) = host.operation.try_lock() {
                            if !host.recovery_pending() {
                                app.exit(0);
                                return;
                            }
                        }
                        show_main(app);
                        let _ = app.emit("desktop-exit-request", ());
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                        show_main(tray.app_handle());
                    }
                })
                .build(app)?;
            backend::start_maintenance(host);
            let update_state = update::state_path();
            std::thread::spawn(move || {
                if let Some(state) = update_state {
                    update::remove_leftovers(&state);
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                if window.label() == "main" {
                    // Errors leave the client alive; never fall back to an unconditional exit.
                    let _ = close_main(window.app_handle());
                }
            }
        })
        .invoke_handler(tauri::generate_handler![api, native])
        .run(tauri::generate_context!())
        .expect("Cannot start Superkiro native host");
}

#[cfg(test)]
mod screen_tests {
    use super::*;

    fn fit(
        scale: f64,
        origin: (i32, i32),
        work_size: (u32, u32),
        position: (i32, i32),
        frame: (u32, u32),
    ) -> ScreenBounds {
        screen_bounds(
            LogicalSize::new(620.0, 820.0),
            scale,
            PhysicalRect {
                position: origin.into(),
                size: work_size.into(),
            },
            position.into(),
            frame.into(),
        )
        .unwrap()
    }

    #[test]
    fn preserves_in_bounds_position_without_centering() {
        let bounds = fit(1.0, (0, 0), (1920, 1040), (123, 87), (0, 0));
        assert_eq!(bounds.size, PhysicalSize::new(620, 820));
        assert_eq!(bounds.position, PhysicalPosition::new(123, 87));
        assert_eq!(bounds.min_size, PhysicalSize::new(480, 540));
    }

    #[test]
    fn clamps_height_and_bottom_on_small_display() {
        let bounds = fit(1.0, (0, 0), (1366, 728), (800, 100), (0, 0));
        assert_eq!(bounds.size, PhysicalSize::new(620, 728));
        assert_eq!(bounds.position, PhysicalPosition::new(746, 0));
    }

    #[test]
    fn handles_scaled_negative_monitor_origin_and_taskbar() {
        let bounds = fit(1.5, (-1920, 40), (1920, 1000), (-1900, 80), (0, 0));
        assert_eq!(bounds.size, PhysicalSize::new(930, 1000));
        assert_eq!(bounds.min_size, PhysicalSize::new(720, 810));
        assert_eq!(bounds.position, PhysicalPosition::new(-1900, 40));
    }

    #[test]
    fn shrinks_minimum_and_accounts_for_outer_frame() {
        let bounds = fit(2.0, (100, -600), (800, 600), (20, -700), (16, 8));
        assert_eq!(bounds.size, PhysicalSize::new(784, 592));
        assert_eq!(bounds.min_size, bounds.size);
        assert_eq!(bounds.position, PhysicalPosition::new(100, -600));
    }

    #[test]
    fn login_uses_its_own_size_and_preserves_position() {
        let bounds = screen_bounds(
            LogicalSize::new(480.0, 540.0),
            1.25,
            PhysicalRect {
                position: (0, 0).into(),
                size: (1920, 1040).into(),
            },
            (200, 100).into(),
            (0, 0).into(),
        )
        .unwrap();
        assert_eq!(bounds.size, PhysicalSize::new(600, 675));
        assert_eq!(bounds.position, PhysicalPosition::new(200, 100));
    }

    #[test]
    fn rejects_invalid_monitor_geometry() {
        for (scale, size) in [(0.0, (800, 600)), (f64::NAN, (800, 600)), (1.0, (0, 600))] {
            assert!(screen_bounds(
                LogicalSize::new(620.0, 820.0),
                scale,
                PhysicalRect {
                    position: (0, 0).into(),
                    size: size.into()
                },
                (0, 0).into(),
                (0, 0).into(),
            )
            .is_err());
        }
    }
}
