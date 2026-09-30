fn main() {
    // tauri-build declares its own inputs, which turns off cargo's default
    // "rerun on any change", and icons are not among them: without this line a
    // new icon.ico is silently not embedded until something else changes.
    println!("cargo:rerun-if-changed=icons");
    tauri_build::build()
}
