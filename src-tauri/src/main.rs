#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use cubby::run_app;

#[path = "bin/win_v_helper.rs"]
mod win_v_helper;

fn main() {
    if std::env::args().any(|arg| arg == "--win-v-helper") {
        if let Err(error) = win_v_helper::run_embedded() {
            eprintln!("Cubby Win+V helper failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    run_app();
}
