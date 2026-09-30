// Release builds are a windowed app: no console window behind the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;

use backend::{Backend, Boot, HostView, StatusEvent};
use input_share_core::config::Home;
use std::sync::{Arc, Mutex};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State, WindowEvent};

struct App(Mutex<Backend>);

fn lock<'a>(app: &'a State<'_, App>) -> std::sync::MutexGuard<'a, Backend> {
    app.0.lock().unwrap_or_else(|e| e.into_inner())
}

#[tauri::command]
fn boot(app: State<App>) -> Result<Boot, String> {
    lock(&app).boot()
}

#[tauri::command]
fn start_sharing(app: State<App>) -> Result<String, String> {
    lock(&app).start_sharing()
}

#[tauri::command]
fn stop_sharing(app: State<App>) {
    lock(&app).stop_sharing()
}

#[tauri::command]
fn start_browsing(app: State<App>) -> Result<(), String> {
    lock(&app).start_browsing()
}

#[tauri::command]
fn hosts(app: State<App>) -> Result<Vec<HostView>, String> {
    lock(&app).hosts()
}

#[tauri::command]
fn stop_browsing(app: State<App>) {
    lock(&app).stop_browsing()
}

#[tauri::command]
fn connect(app: State<App>, addr: String) -> Result<(), String> {
    lock(&app).connect(&addr)
}

#[tauri::command]
fn disconnect(app: State<App>) {
    lock(&app).disconnect()
}

#[tauri::command]
fn key_generate(app: State<App>, replace: bool) -> Result<String, String> {
    lock(&app).key_generate(replace)
}

#[tauri::command]
fn key_import(app: State<App>, text: String, replace: bool) -> Result<String, String> {
    lock(&app).key_import(&text, replace)
}

#[tauri::command]
fn key_reveal(app: State<App>) -> Result<String, String> {
    lock(&app).key_reveal()
}

#[tauri::command]
fn save_settings(app: State<App>, edge: String, port: u16) -> Result<(), String> {
    lock(&app).save_settings(&edge, port)
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
        "stop" => {
            lock(&app.state::<App>()).shutdown();
            let _ = app.emit("tray-stop", ());
        }
        "quit" => {
            lock(&app.state::<App>()).shutdown(); // release everything before exiting
            app.exit(0);
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

/// `--demo-tray stop|close-show|close-idle`: drive the tray and window the
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
            // while idle it quits.
            if let WindowEvent::CloseRequested { api, .. } = event
                && lock(&window.state::<App>()).is_running()
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
