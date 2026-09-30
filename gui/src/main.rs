// Release builds are a windowed app: no console window behind the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;

use backend::{Backend, Boot, HostView, StatusEvent};
use input_share_core::config::Home;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager, State};

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

/// Value after `--name`, if present.
fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let demo = args.iter().any(|a| a == "--demo");
    let demo_state = flag(&args, "--demo-state");
    let theme = flag(&args, "--demo-theme").filter(|t| t == "light" || t == "dark");

    tauri::Builder::default()
        .setup(move |app| {
            let home = if demo {
                // Demo never touches the real %APPDATA% folder.
                let home = Home::at(std::env::temp_dir().join("input-share-demo"))?;
                if home.load_key()?.is_none() {
                    home.generate_key(false)?;
                }
                home
            } else {
                Home::user()?
            };
            let handle = app.handle().clone();
            let on_status = Arc::new(move |s| {
                let _ = handle.emit("status", StatusEvent::from(s));
            });
            app.manage(App(Mutex::new(Backend::new(home, demo, demo_state, theme, on_status))));
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
            save_settings
        ])
        .on_window_event(|window, event| {
            // Closing the window stops everything cleanly (item 7 adds the tray).
            if let tauri::WindowEvent::Destroyed = event {
                lock(&window.state::<App>()).shutdown();
            }
        })
        .run(tauri::generate_context!())
        .expect("failed to start the GUI");
}
