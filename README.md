# keytool

A native configurator for the USB HID macro keypad (VID `0x1189` / `0x514C`,
PID family `0x883x` / `0x884x`). It replaces the original Windows-only Qt5/HID
app with a cross-platform Rust application — a CLI for protocol work and
scripting, and an egui GUI for everyday use.

The full reverse-engineered protocol is documented in
[`../FINDINGS.md`](../FINDINGS.md); this crate implements it.

## What it does

- **Connect** to the keypad over HID (same `hidapi` C library the original app used).
- **Read** the full configuration: up to 12 keys × 3 layers, with mode, delay,
  and up to 18 slots per key.
- **Edit** key mappings: basic keys (keycode + modifiers Ctrl/Shift/Alt/Win),
  function keys, multimedia, mouse actions, RGB LED, and per-key delay.
- **Set the layer-indicator LED** — a per-layer colour (red..purple) that flashes
  a key on layer switch. Confirmed on hardware; see
  [`../LED_INVESTIGATION.md`](../LED_INVESTIGATION.md) for the protocol summary.
- **Write** the configuration back to the device and commit it.

The protocol is **proven against real hardware**: both reads and a
restorable write→read→restore round-trip have been validated on a 12-key,
3-layer unit.

## Requirements

- Rust 1.96+ (cargo).
- The native `hidapi` library. The `hidapi` crate builds a bundled backend, so
  on macOS no system install is needed. On Linux you may need
  `libhidapi-dev`/`libudev-dev` and udev rules for device access; on Windows it
  links the bundled DLL.
- A keypad plugged in. macOS may grab the device's keyboard interface; `keytool`
  probes each composite interface and keeps the one that responds.

## Building

```sh
# CLI only (default)
cargo build --release --bin keytool

# GUI (requires the `gui` feature)
cargo build --release --bin keytool-gui --features gui
```

## Running the GUI

```sh
cargo run --release --bin keytool-gui --features gui
```

1. Plug in the keypad.
2. Click **Connect** — it enumerates the device and auto-reads the config.
3. Pick a layer (L1–L3) and click a key to edit it. Change mode, slots, delay.
4. Click **Write** to push changes (only modified keys are written) and commit.

Modified keys are highlighted; the Write button is disabled until something
has changed. The status line reports connection state, key/layer count, and
errors. HID I/O runs on a background thread so the UI never freezes.

> **Note:** the keypad has a physical mode switch that can make the `0xFB`
> query report a degenerate key count (`2 keys / 0 layers`) while the full
> config is still stored. `keytool` always force-reads 12 keys × 3 layers, so
> the config loads correctly regardless of the switch position.

## CLI reference

Run with `cargo run --release --bin keytool -- <command>`.

| Command | Description |
|---|---|
| `list` | Enumerate matching keypads (VID/PID). |
| `info` | Open the device and read its key/layer count. |
| `read` | Read every key and dump a hex table. |
| `label` | Read every key and print human-readable labels (`Ctrl+C`, `F5`, …). |
| `raw` | Dump the raw 64-byte `0xFB` key-count response. |
| `rawkey [b] [c] [counter]` | Send `[03 FA b c counter]` and dump the raw response (debug). |
| `stream [b] [c] [counter]` | Send a read-key report and dump *all* streamed packets. |
| `readlayer [layer]` | Read one layer (0-indexed) and print labels. |
| `dump [keys] [layers]` | Force-read 12 keys × 3 layers; dump hex + label per key. Good for backups. |
| `writetest [key] [layer]` | Restorable write-modify-read-restore round-trip on one key. |
| `writeprobe [key] [layer]` | Probe whether the device ACKs a write/commit (drains responses). |
| `brutewrite [key] [layer]` | Brute-force the write command byte against a disposable key. |
| `setkey [key] [layer] [code]…` | Write a basic-mode key directly (`code` = HID usage ID, e.g. `0x0c`=`i`). |
| `setmedia [slot] [layer] [code]` | Write a multimedia key (mode 2) directly (`code` = consumer usage ID). |
| `set-led [layer] [colour]` | Set the layer-indicator LED. `layer` 1..3 (device layer), `colour` = name (`red`/`orange`/`yellow`/`green`/`cyan`/`blue`/`purple`) or index 1..7. |
| `smoke [key] [layer]` | End-to-end test of the GUI's device stack (connect→read→write→restore). |

## Quick start

```sh
# What's connected?
cargo run --release --bin keytool -- info

# Load and view the config
cargo run --release --bin keytool -- label

# Back up the full config to a file
cargo run --release --bin keytool -- dump 12 3 > config-backup.txt

# Verify the whole read→write→restore stack works on your hardware
cargo run --release --bin keytool -- smoke 1 1

# Give each layer's indicator a distinct colour (red / green / blue)
cargo run --release --bin keytool -- set-led 1 red
cargo run --release --bin keytool -- set-led 2 green
cargo run --release --bin keytool -- set-led 3 blue
```

## Protocol summary

65-byte HID reports: report ID `0x03` + 64 payload bytes.

| Marker | Command | Report |
|---|---|---|
| `0xFB` | read key count/layers | `[03, FB, FB, FB, …]` → resp `[03, FB, keys, layers]` |
| `0xFA` | read a layer (streams `keyCount` packets) | `[03, FA, keyCount, 0, layer(1-idx)]` |
| `0xFE` | **write one key** | `[03, FE, key(1-idx), layer(1-idx), MODE, DELAY, 0,0,0,0, SEQ_LEN, slots…]` |
| `0xFE` + `[2]=0xB0` | **write layer-indicator LED** | `[03, FE, B0, layer(1-idx), 08, 0…, 01, 00, <colour>]` |
| `0xFD` | commit after writes | `[03, FD, FE, FF, …]` |
| `0xFC` | set keyboard version (optional) | `[03, FC, FC, 02, 00, …]` |

Per-key record: 50 bytes — `record[1]`=key (1-indexed), `record[2]`=layer
(1-indexed), `record[3]`=MODE, `record[4]`=DELAY, `record[9]`=sequence length,
`record[0x0A..0x2D]`=18 × 2-byte slots. Modes: 1=basic, 2=multimedia, 3=mouse,
5=LED, 8=macro (stored/loaded opaquely).

## Project layout

```
src/
  protocol.rs   wire protocol + 50-byte record (de)serialization
  device.rs     hidapi wrapper: enumerate / open / read / write
  worker.rs     background HID thread (Cmd/Event channels)
  hid_codes.rs  USB HID keyboard usage-code name tables
  app.rs        egui application (gui feature)
  bin/
    cli.rs      the keytool CLI
    gui.rs      GUI entry point
```

## Status & limitations

- Read and write paths are hardware-validated; the egui UI is functional.
- **Mode 8 (macro)** payload format is not fully decoded — it is read and
  written as opaque bytes so existing macros are never corrupted.
- **Firmware update** (`KB_UpData_SoftWare`) is a separate protocol, not
  implemented.
- The `0x1189` variant (the original app's target) is supported by
  disassembly but not hardware-tested here; the `0x514C` unit is the validated
  target.
