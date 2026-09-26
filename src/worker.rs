//! Background HID worker.
//!
//! HID I/O must not block the GUI thread, so the device handle lives on a
//! dedicated worker thread. The UI sends [`Cmd`]s and receives [`Event`]s over
//! `crossbeam-channel` endpoints; the worker runs an event loop that performs
//! the (possibly slow) device operations and reports results back.

use std::thread;

use crossbeam_channel::{bounded, Receiver, Sender};

use crate::device::{Device, DeviceError};
use crate::protocol::{KeyCount, KeyRecord, ROTARY_COUNT};

/// One record to write: 1-indexed key, 0-indexed layer, and the record bytes.
pub type WriteItem = (u8, u8, KeyRecord);

/// Commands the UI sends to the worker.
pub enum Cmd {
    /// Open the keypad. Replies with [`Event::Connected`] or [`Event::Error`].
    Connect,
    /// Close the current connection (if any).
    Disconnect,
    /// Read the full config (all keys × all layers). Replies with
    /// [`Event::Config`].
    Read,
    /// Write the given records back to the device and commit. Replies with
    /// [`Event::Written`].
    Write(Vec<WriteItem>),
    /// Write an LED layer-indicator record + commit. Layer is 1-indexed (1..3),
    /// colour index 1..7 (red..purple). Replies with [`Event::Written`].
    WriteLed(u8, u8),
    /// Stop the worker thread.
    Quit,
}

/// Events the worker sends back to the UI.
#[derive(Debug)]
pub enum Event {
    /// Connected; reports the key/layer count from the `0xFB` query.
    Connected(KeyCount),
    /// Disconnected (requested or because the device went away).
    Disconnected,
    /// A full config read completed. `layers[l]` is a vector indexed by
    /// 1-indexed key (slot 0 reserved).
    Config(Vec<Vec<KeyRecord>>),
    /// A write completed successfully.
    Written,
    /// A read completed but the device's `0xFB` reported a degenerate key count
    /// (the physical mode switch can make it report 2/0 while the config
    /// persists — FINDINGS §11.8). The read still used the forced 12-key form.
    DegenerateKeyCount,
    /// An operation failed.
    Error(String),
}

/// A handle to the worker: the command channel plus the join handle.
pub struct Worker {
    pub cmd_tx: Sender<Cmd>,
    pub event_rx: Receiver<Event>,
    pub join: Option<thread::JoinHandle<()>>,
}

impl Worker {
    /// Spawn the worker thread. It starts disconnected.
    pub fn spawn() -> Self {
        let (cmd_tx, cmd_rx) = bounded(64);
        let (event_tx, event_rx) = bounded(64);
        let join = thread::Builder::new()
            .name("keytool-hid".into())
            .spawn(move || run(cmd_rx, event_tx))
            .ok();
        Self {
            cmd_tx,
            event_rx,
            join,
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.cmd_tx.send(cmd);
    }

    /// Signal the worker to quit and join it.
    pub fn stop(&mut self) {
        self.send(Cmd::Quit);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn run(cmd_rx: Receiver<Cmd>, event_tx: Sender<Event>) {
    let mut device: Option<Device> = None;
    let mut degenerate = false;

    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            Cmd::Connect => match Device::open() {
                Ok(d) => {
                    // Device::open already pinged to pick the right interface;
                    // re-pinging here can stall the subsequent streaming read on
                    // the degenerate-mode device, so report a default count and
                    // let Read force 12 keys regardless. The reported count is
                    // informational (the mode switch makes it unreliable).
                    let kc = KeyCount { keys: 12, layers: 3 };
                    degenerate = false;
                    let _ = event_tx.send(Event::Connected(kc));
                    device = Some(d);
                }
                Err(e) => {
                    let _ = event_tx.send(Event::Error(format!("{e}")));
                }
            },
            Cmd::Disconnect => {
                device = None;
                degenerate = false;
                let _ = event_tx.send(Event::Disconnected);
            }
            Cmd::Read => {
                let Some(d) = device.as_ref() else {
                    let _ = event_tx.send(Event::Error("not connected".into()));
                    continue;
                };
                match read_full_config(d, degenerate) {
                    Ok((layers, degenerate)) => {
                        let _ = event_tx.send(Event::Config(layers));
                        if degenerate {
                            let _ = event_tx.send(Event::DegenerateKeyCount);
                        }
                    }
                    Err(e) => {
                        let _ = event_tx.send(Event::Error(format!("read failed: {e}")));
                    }
                }
            }
            Cmd::Write(items) => {
                let Some(d) = device.as_ref() else {
                    let _ = event_tx.send(Event::Error("not connected".into()));
                    continue;
                };
                match write_records(d, &items) {
                    Ok(()) => {
                        let _ = event_tx.send(Event::Written);
                    }
                    Err(e) => {
                        let _ = event_tx.send(Event::Error(format!("write failed: {e}")));
                    }
                }
            }
            Cmd::WriteLed(layer, colour_idx) => {
                let Some(d) = device.as_ref() else {
                    let _ = event_tx.send(Event::Error("not connected".into()));
                    continue;
                };
                match d.write_led_and_commit(layer, colour_idx) {
                    Ok(()) => {
                        let _ = event_tx.send(Event::Written);
                    }
                    Err(e) => {
                        let _ = event_tx.send(Event::Error(format!("LED write failed: {e}")));
                    }
                }
            }
            Cmd::Quit => break,
        }
    }
}

/// Force-read 12 keys + 3 rotary encoders × 3 layers (the config persists even
/// when the mode switch makes `0xFB` report 2/0 — FINDINGS §11.8). Returns
/// `(layers, degenerate)`.
///
/// The key/layer count is *not* re-queried here — the caller already pinged at
/// connect time, and re-pinging mid-sequence can stall the streaming `0xFA`
/// read on the degenerate-mode device. We always request 12 keys + 3 rotary
/// encoders (`rotary_count = 3`, verified against the `12add3` hardware) × 3
/// layers — 21 packets per layer, slots 1..=12 then 16..=24. A degenerate-mode
/// device may stream only the 12 key packets; `read_layer`'s timed loop then
/// leaves the rotary slots empty and the GUI carries prior rotary state over so
/// local edits survive.
fn read_full_config(d: &Device, degenerate: bool) -> Result<(Vec<Vec<KeyRecord>>, bool), DeviceError> {
    let layers = 3;
    let mut out = Vec::with_capacity(layers);
    for layer in 0..layers {
        out.push(d.read_layer(12, ROTARY_COUNT as u8, layer as u8)?);
    }
    Ok((out, degenerate))
}

/// Write each record (1-indexed key, 0-indexed layer) with its own commit. The
/// `0xFE` write marker + key/layer addressing are encoded by the device layer.
fn write_records(d: &Device, items: &[WriteItem]) -> Result<(), DeviceError> {
    for &(key, layer, ref rec) in items {
        d.write_and_commit(key, layer, rec)?;
    }
    Ok(())
}
