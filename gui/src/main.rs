// Release builds are a windowed app: no console window behind the GUI.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default().run(tauri::generate_context!()).expect("failed to start the GUI");
}
