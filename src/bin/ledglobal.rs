//! Probe for global/reactive LED settings.
//!
//! Observation: Layer 2 keys flash GREEN on press/release, but slot-0 LED records
//! are empty (mode=0). The reactive LED mode must be stored elsewhere.
//!
//! Possibilities:
//! 1. A special "global" slot outside the normal 1-24 range
//! 2. A separate HID command for reactive mode
//! 3. Factory default that persists until explicitly changed
//!
//! This probes:
//! - Slot 0xFF or other special slots
//! - Different record[1] values (0xB0 is per-layer, maybe 0xC0 is global?)
//! - The 0xFC (set version) command with different parameters

use std::time::Duration;

use keytool::{
    device::Device,
    protocol::{self, off, REPORT_LEN},
};

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn try_send(d: &Device, report: &[u8; REPORT_LEN], label: &str) {
    print!("  {label}: ");
    match d.debug_send(report) {
        Ok(()) => {
            println!("OK  {}", hex(&report[1..8]));
        }
        Err(e) => println!("ERR {e}"),
    }
    std::thread::sleep(Duration::from_millis(30));
}

fn main() {
    let d = match Device::open() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error opening device: {e}");
            std::process::exit(1);
        }
    };
    println!("opened device\n");

    // The device shows green reactive flash on layer 2.
    // Let's try to find where this is configured.

    println!("=== Probing global LED commands ===\n");

    // Try different record[1] values (0xB0 = per-layer, try 0xC0, 0xD0, 0xE0 for global)
    println!("--- Trying different LED sub-markers ---");
    for marker in [0xB0u8, 0xC0, 0xD0, 0xE0, 0xF0] {
        for mode in 0u8..=3 {
            let mut report = [0u8; REPORT_LEN];
            report[0] = 0x03;
            let rec = &mut report[1..51];
            rec[0x00] = 0xFE;
            rec[0x01] = marker;
            rec[0x02] = mode;
            rec[0x03] = 5; // LED mode
            rec[off::SEQ_LEN] = 1;
            rec[off::LED_BITMASK] = 0x0F; // all keys, group 0

            try_send(&d, &report, &format!("marker=0x{marker:02X} mode={mode}"));
            let _ = d.debug_send(&protocol::commit_report());
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Try slot 0xFF (special "global" slot?)
    println!("\n--- Trying special slots ---");
    for slot in [0x00u8, 0xFF, 0xFE, 0xFD] {
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        let rec = &mut report[1..51];
        rec[0x00] = 0xFE;
        rec[0x01] = slot; // try as slot number
        rec[0x02] = 0;    // layer 0
        rec[0x03] = 5;
        rec[off::SEQ_LEN] = 1;
        rec[off::LED_BITMASK] = 0x0F;

        try_send(&d, &report, &format!("slot=0x{slot:02X}"));
        let _ = d.debug_send(&protocol::commit_report());
        std::thread::sleep(Duration::from_millis(50));
    }

    // Try reading back what we might have changed
    println!("\n--- Reading slot 0 after probes ---");
    match d.read_layer(12, 3, 0) {
        Ok(row) => {
            let slot0 = &row[0];
            println!("Layer 1 slot 0: mode={} raw={}", slot0.mode, hex(&slot0.to_bytes()[..12]));
        }
        Err(e) => println!("Read error: {e}"),
    }

    // Try the FC command with different version/mode values
    println!("\n--- Trying FC (set version) with LED parameters ---");
    for param in 0u8..=5 {
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        report[1] = 0xFC;
        report[2] = 0xFC;
        report[3] = param; // try mode here
        report[4] = 0x00;

        try_send(&d, &report, &format!("FC param={param}"));
    }

    // Try a "disable reactive" pattern
    println!("\n--- Trying to disable/enable reactive mode ---");

    // Maybe 0xB0 with specific parameters controls reactive mode?
    for reactive_mode in 0u8..=5 {
        let mut report = [0u8; REPORT_LEN];
        report[0] = 0x03;
        report[1] = 0xB0;
        report[2] = reactive_mode;
        report[3] = 0x00;
        report[4] = 0x00;

        try_send(&d, &report, &format!("B0 reactive_mode={reactive_mode}"));
        let _ = d.debug_send(&protocol::commit_report());
        std::thread::sleep(Duration::from_millis(100));

        println!("    >>> Now press a key on layer 2. Still green? <<<");
    }

    println!("\n=== Summary ===");
    println!("The reactive green flash is independent of slot-0 LED records.");
    println!("Try pressing keys on layer 2 after each section above to see if any changed the colour.");
    println!("\nIf the green flash persists through all probes, it may be:");
    println!("  1. A factory default stored in firmware");
    println!("  2. Controlled by a command we haven't found yet");
    println!("  3. Set via the Windows app's Dialog3 (needs USB capture to find)");
}
