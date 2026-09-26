//! keytool — configurator for the 0x1189 USB HID macro keypad.
//!
//! Three layers:
//!   - [`protocol`] — pure wire-format logic (report framing, 50-byte records).
//!   - [`device`]   — thin `hidapi` wrapper (enumerate/open/read/write).
//!   - [`hid_codes`] — USB HID usage-code name tables.
//!
//! The GUI lives in `src/bin/gui.rs`; the minimal CLI in `src/bin/cli.rs`.

pub mod device;
pub mod hid_codes;
pub mod protocol;
pub mod worker;

#[cfg(feature = "gui")]
pub mod app;

#[cfg(feature = "gui")]
pub mod config;
