// Windows release builds must not spawn a console window alongside the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    yawm_desktop_lib::run()
}
