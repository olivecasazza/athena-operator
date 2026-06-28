//! Frontend entrypoint — launches the Dioxus web app. The native backend lives
//! in `src/bin/server.rs` behind the `server` feature.

fn main() {
    dioxus::launch(athena_console_web::App);
}
