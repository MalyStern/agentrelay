// The Windows subsystem line keeps a console window from flashing up behind the app
// in a release build — a small thing that separates "a real desktop app" from "a
// terminal program with a window".
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    agentrelay_app_lib::run()
}
