// Release builds are a windowed app: no console window behind the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;

use backend::{Backend, Boot, HostView, StatusEvent};
use input_share_core::config::Home;
use std::sync::{Arc, Mutex, TryLockError};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

struct App(Mutex<Backend>);

fn lock<'a>(app: &'a State<'_, App>) -> std::sync::MutexGuard<'a, Backend> {
    app.0.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run `f` on the backend from the blocking pool, never the main thread. A
/// stop waits for network threads, which can take seconds; on the main thread
/// that froze the window until Windows closed it as hung (BUG-004).
async fn on_backend<T: Send + 'static>(app: AppHandle, f: impl FnOnce(&mut Backend) -> T + Send + 'static) -> Result<T, String> {
    tauri::async_runtime::spawn_blocking(move || f(&mut lock(&app.state::<App>()))).await.map_err(|e| e.to_string())
}

#[tauri::command]
async fn boot(app: AppHandle) -> Result<Boot, String> {
    on_backend(app, |b| b.boot()).await?
}

#[tauri::command]
async fn start_sharing(app: AppHandle) -> Result<String, String> {
    on_backend(app, |b| b.start_sharing()).await?
}

#[tauri::command]
async fn stop_sharing(app: AppHandle) -> Result<(), String> {
    on_backend(app, |b| b.stop_sharing()).await
}

#[tauri::command]
async fn start_browsing(app: AppHandle) -> Result<(), String> {
    on_backend(app, |b| b.start_browsing()).await?
}

#[tauri::command]
async fn hosts(app: AppHandle) -> Result<Vec<HostView>, String> {
    on_backend(app, |b| b.hosts()).await?
}

#[tauri::command]
async fn stop_browsing(app: AppHandle) -> Result<(), String> {
    on_backend(app, |b| b.stop_browsing()).await
}

#[tauri::command]
async fn connect(app: AppHandle, addr: String) -> Result<(), String> {
    on_backend(app, move |b| b.connect(&addr)).await?
}

#[tauri::command]
async fn disconnect(app: AppHandle) -> Result<(), String> {
    on_backend(app, |b| b.disconnect()).await
}

#[tauri::command]
async fn key_generate(app: AppHandle, replace: bool) -> Result<String, String> {
    on_backend(app, move |b| b.key_generate(replace)).await?
}

#[tauri::command]
async fn key_import(app: AppHandle, text: String, replace: bool) -> Result<String, String> {
    on_backend(app, move |b| b.key_import(&text, replace)).await?
}

#[tauri::command]
async fn key_reveal(app: AppHandle) -> Result<String, String> {
    on_backend(app, |b| b.key_reveal()).await?
}

#[tauri::command]
async fn save_settings(app: AppHandle, edge: String, port: u16) -> Result<(), String> {
    on_backend(app, move |b| b.save_settings(&edge, port)).await?
}

/// The web UI knows the whole state (role, where the pointer is, and which
/// side each computer is drawn on), so it tells the tray what to show:
/// "idle", or "here-" / "away-" plus the side of the lit screen.
#[tauri::command]
fn tray_state(app: tauri::AppHandle, state: String, tooltip: String) -> Result<(), String> {
    let icon = match state.as_str() {
        "here-left" => include_bytes!("../icons/tray-here-left.png").as_slice(),
        "here-right" => include_bytes!("../icons/tray-here-right.png").as_slice(),
        "away-left" => include_bytes!("../icons/tray-away-left.png").as_slice(),
        "away-right" => include_bytes!("../icons/tray-away-right.png").as_slice(),
        _ => include_bytes!("../icons/tray-idle.png").as_slice(),
    };
    let tray = app.tray_by_id(TRAY).ok_or("no tray icon")?;
    tray.set_icon(Some(Image::from_bytes(icon).map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?;
    tray.set_tooltip(Some(tooltip)).map_err(|e| e.to_string())
}

const TRAY: &str = "main";

fn show_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// The tray menu's actions. Demo mode drives the same function.
fn tray_action(app: &AppHandle, id: &str) {
    match id {
        "show" => show_window(app),
        // Off the main thread, like the commands: stopping can take seconds.
        "stop" => {
            let app = app.clone();
            std::thread::spawn(move || {
                lock(&app.state::<App>()).shutdown();
                let _ = app.emit("tray-stop", ());
            });
        }
        "quit" => {
            let app = app.clone();
            std::thread::spawn(move || {
                lock(&app.state::<App>()).shutdown(); // release everything (cursor, keys) before exiting
                app.exit(0);
            });
        }
        _ => {}
    }
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Show input-share", true, None::<&str>)?;
    let stop = MenuItem::with_id(app, "stop", "Stop", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &stop, &quit])?;
    TrayIconBuilder::with_id(TRAY)
        .icon(Image::from_bytes(include_bytes!("../icons/tray-idle.png"))?)
        .tooltip("input-share: not sharing")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, e| tray_action(app, e.id.as_ref()))
        .on_tray_icon_event(|tray, e| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                show_window(tray.app_handle());
            }
        })
        .build(app)?;
    Ok(())
}

/// `--demo-tray stop|quit|close-show|close-idle`: drive the tray and window the
/// way a user would, so screenshots can check the result. Demo only.
fn demo_tray(app: AppHandle, sequence: String) {
    let pause = |ms| std::thread::sleep(std::time::Duration::from_millis(ms));
    let window = || app.get_webview_window("main");
    pause(800);
    if sequence != "close-idle" {
        let _ = lock(&app.state::<App>()).start_sharing();
    }
    pause(800);
    match sequence.as_str() {
        "stop" => {
            // Hold the sharing view long enough to be captured before the stop.
            pause(1900);
            tray_action(&app, "stop");
        }
        "quit" => tray_action(&app, "quit"),
        "close-show" | "close-idle" => {
            if let Some(w) = window() {
                let _ = w.close(); // goes through CloseRequested, like the title bar's X
            }
            pause(800);
            if sequence == "close-show" {
                tray_action(&app, "show");
            }
        }
        _ => {}
    }
}

/// Value after `--name`, if present.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    let demo_state = flag(&args, "--demo-state");
    let theme = flag(&args, "--demo-theme").filter(|t| t == "light" || t == "dark");
    let demo_tray_seq = flag(&args, "--demo-tray").filter(|_| demo);

    // One copy at a time (BUG-001): launching again brings the running copy
    // forward, even from the tray, instead of opening a second idle window
    // next to one that is still sharing. Demo mode is exempt so its checks can
    // run beside the real app. If the check itself fails, start anyway.
    let instance = if demo {
        None
    } else {
        match input_share_core::win::single_instance(r"Local\input-share-gui") {
            Ok(Some(first)) => Some(first),
            Ok(None) => return, // the running copy has been asked to show itself
            Err(_) => None,
        }
    };

    tauri::Builder::default()
        .setup(move |app| {
            build_tray(app)?;
            let home = if demo {
                // Demo never touches the real %APPDATA% folder.
                let home = Home::at(std::env::temp_dir().join("input-share-demo"))?;
                if home.load_key()?.is_none() {
                    home.generate_key(false)?;
                }
                home
            } else {
                // If an earlier run crashed while the parked cursor was hidden,
                // bring it back now rather than only when sharing starts.
                input_share_core::cursor::restore();
                Home::user()?
            };
            let handle = app.handle().clone();
            let on_status = Arc::new(move |s| {
                let _ = handle.emit("status", StatusEvent::from(s));
            });
            app.manage(App(Mutex::new(Backend::new(home, demo, demo_state, theme, on_status))));
            if let Some(seq) = demo_tray_seq.clone() {
                let handle = app.handle().clone();
                std::thread::spawn(move || demo_tray(handle, seq));
            }
            if let Some(first) = instance {
                let handle = app.handle().clone();
                std::thread::spawn(move || {
                    while first.wait() {
                        show_window(&handle);
                    }
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            boot,
            start_sharing,
            stop_sharing,
            start_browsing,
            hosts,
            stop_browsing,
            connect,
            disconnect,
            key_generate,
            key_import,
            key_reveal,
            save_settings,
            tray_state
        ])
        .on_window_event(|window, event| {
            // While sharing or connected, the close button hides to the tray;
            // while idle it quits. Busy (a start or stop in progress) counts as
            // running: waiting for the lock here would freeze the window.
            if let WindowEvent::CloseRequested { api, .. } = event
                && match window.state::<App>().0.try_lock() {
                    Ok(b) => b.is_running(),
                    Err(TryLockError::Poisoned(e)) => e.into_inner().is_running(),
                    Err(TryLockError::WouldBlock) => true,
                }
            {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .build(tauri::generate_context!())
        .expect("failed to start the GUI")
        .run(|app, event| {
            // Every way out (last window closed, tray Quit, logoff) stops
            // cleanly: the client releases keys, the server unhooks.
            if let RunEvent::Exit = event {
                lock(&app.state::<App>()).shutdown();
            }
        });
}
