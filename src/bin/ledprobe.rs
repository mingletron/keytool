//! Probe for the global LED enable command.
//!
//! The per-key LED records are accepted but don't light up. There's likely
//! a global "LED mode" command that enables/disables the LED system.
//! This probes candidate report patterns to find it.

use std::io::Write;
use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, REPORT_LEN},
};

fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}

/// Try sending a report and checking if it's accepted (no error)
fn try_send(d: &Device, report: &[u8; REPORT_LEN], label: &str) -> bool {
    match d.debug_send(report) {
        Ok(()) => {
            println!("  {label}: OK  raw={}", hex(&report[1..10]));
            true
        }
        Err(e) => {
            println!("  {label}: ERR {e}");
            false
        }
    }
}

fn main() {
    let d = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening device: {e}");
            std::process::exit(1);
        }
    };
    println!("opened device");

    // First, write a known LED record so we have something to observe
    println!("\n--- Writing test LED indicator (layer 1, key 1, group 0) ---");
    let mut led_report = [0u8; REPORT_LEN];
    led_report[0] = 0x03;
    led_report[1] = 0xFE; // write marker
    led_report[2] = 0xB0; // LED sub-marker
    led_report[3] = 1;    // layer 1
    led_report[4] = 5;    // mode = LED
    led_report[9] = 1;    // seq_len
    led_report[0x0B] = 0x01; // bitmask: group 0, key 1
    let _ = d.debug_send(&led_report);
    std::thread::sleep(Duration::from_millis(30));
    let _ = d.debug_send(&protocol::commit_report());
    println!("LED record written. Now probing enable commands...\n");

    // Probe candidate LED enable commands
    // Format guess: [0x03, cmd, mode_value, ...] where mode 0=off, 1=on, etc.

    let mut stdout = std::io::stdout();

    // Try various command bytes
    println!("=== Probing command bytes ===");
    for cmd in [0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF] {
        for mode in 0u8..=5 {
            let mut report = [0u8; REPORT_LEN];
            report[0] = 0x03;
            report[1] = cmd;
            report[2] = mode;
            // Try with different second bytes
            report[3] = mode;
            let label = format!("cmd=0x{cmd:02X} mode={mode}");
            if try_send(&d, &report, &label) {
                // Wait and observe
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }

    // Try the pattern from Dialog3: 0xB0 combined with mode
    println!("\n=== Probing 0xB0-based patterns ===");
    for mode in 0u8..=5 {
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        report[1] = 0xB0; // LED marker
        report[2] = mode;
        report[3] = 0x00;
        let label = format!("0xB0 mode={mode}");
        if try_send(&d, &report, &label) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // Try FE with 0xB0 at position 2 (like LED record but to slot 0)
    println!("\n=== Probing FE+B0 slot-0 patterns ===");
    for mode in 0u8..=5 {
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        report[1] = 0xFE;
        report[2] = 0xB0; // might select "global LED" instead of per-layer
        report[3] = 0x00; // slot 0 / layer 0
        report[4] = mode; // LED mode
        report[9] = 1;
        report[0x0B] = mode;
        let label = format!("FE+B0 slot0 mode={mode}");
        if try_send(&d, &report, &label) {
            let _ = d.debug_send(&protocol::commit_report());
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // Try FC (set version command) with LED mode values
    println!("\n=== Probing FC (set version) patterns ===");
    for mode in 0u8..=5 {
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        report[1] = 0xFC;
        report[2] = 0xFC;
        report[3] = mode; // try mode in byte 3
        report[4] = 0x00;
        let label = format!("FC mode={mode}");
        if try_send(&d, &report, &label) {
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    // Try raw mode values at different offsets
    println!("\n=== Probing raw mode bytes ===");
    for offset in 1..=5 {
        for mode in 0u8..=5 {
            let mut report = [0u8; REPORT_LEN];
            report[0] = 0x03;
            report[offset] = mode;
            let label = format!("byte[{offset}]={mode}");
            if try_send(&d, &report, &label) {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }

    println!("\n=== Done ===");
    println!("Watch the device LEDs during this probe. Note which pattern lit them.");
    let _ = stdout.flush();
}
