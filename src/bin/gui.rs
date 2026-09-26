//! GUI binary for the macro keypad (eframe/egui).
//!
//! Built only with the `gui` feature:
//!   cargo run --bin keytool-gui --features gui --release
//!
//! The app lives in [`keytool::app`]; this is just the entry point. HID I/O runs
//! on a background thread ([`keytool::worker`]) so the GUI never blocks.

#[cfg(not(feature = "gui"))]
fn main() {
    eprintln!(
        "this binary requires the `gui` feature: cargo run --bin keytool-gui --features gui"
    );
}

#[cfg(feature = "gui")]
fn main() -> eframe::Result<()> {
    keytool::app::run()
}
