//! egui application for the macro keypad configurator.
//!
//! Only compiled with the `gui` feature (gated in `lib.rs`).

use std::collections::HashSet;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::device::label_for;
use crate::hid_codes;
use crate::protocol::{self, KeyRecord, Mode, RotaryDir, Slot};
use crate::worker::{Cmd, Event, Worker};

/// Physical key buttons (1-indexed: 1..=KEYS). The device reports 12 keys.
const KEYS: usize = 12;
/// Max addressable slot index (the last rotary slot, 24); slot 0 is a reserved
/// header. Rows are `SLOTS + 1` wide = [`protocol::LAYER_SLOTS`] (25): slots
/// 1..=12 are keys, 13..=15 are dead (hidden 15-key-model keys), and 16..=24 are
/// the three rotary encoders' turn-left / press / turn-right slots.
const SLOTS: usize = protocol::ROTARY_SLOT_END;
const LAYERS: usize = 3;

/// LED colour indices and names, matching the Windows app's sweep
/// (led2.pcapng: groups 1..7 = red..purple, low nibble 0x04).
const LED_COLOURS: &[(usize, &str)] = &[
    (1, "Red"),
    (2, "Orange"),
    (3, "Yellow"),
    (4, "Green"),
    (5, "Cyan"),
    (6, "Blue"),
    (7, "Purple"),
];

/// True for rotary-encoder slots (16..=24).
fn is_rotary(k: usize) -> bool {
    protocol::is_rotary_slot(k)
}

/// Logical slot for each physical key position, in row-major order
/// (top-left first). The device numbers its 12 keys column-major,
/// bottom-to-top — slot 1 is bottom-left, slot 4 top-left, … slot 12
/// top-right. The GUI is laid out for a keypad rotated 90° clockwise
/// (rotaries on the right), which transposes that grid so the labels
/// fall into plain sequential reading order: row 0 = slots 1-4, row 1 =
/// 5-8, row 2 = 9-12. Verified against hardware by pressing each key and
/// reverse-matching its keystroke. Used so a physical press lights the
/// cell under your finger.
const PHYSICAL_ORDER: [usize; KEYS] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnState {
    Disconnected,
    Connecting,
    Connected,
}

/// A device/config action that may need a dirty-state confirmation before it
/// runs (because it would discard unsaved edits). Stored in
/// [`KeyToolApp::pending`] while the inline confirm is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingAction {
    Read,
    Disconnect,
    ClearAll,
}

impl PendingAction {
    fn label(&self) -> &'static str {
        match self {
            PendingAction::Read => "re-read from the device",
            PendingAction::Disconnect => "disconnect",
            PendingAction::ClearAll => "clear all keys",
        }
    }
}

pub struct KeyToolApp {
    worker: Worker,
    conn: ConnState,
    key_count: usize,
    layer_count: usize,
    /// layers[l][k] = record for 1-indexed key k (slot 0 reserved header).
    layers: Vec<Vec<KeyRecord>>,
    /// layers as read from the device (or loaded from file), for dirty detection.
    original: Vec<Vec<KeyRecord>>,
    selected_layer: usize,
    selected_key: usize,
    dirty: HashSet<(u8, u8)>, // (1-indexed key, 0-indexed layer)
    busy: bool,
    status: String,
    pending_degenerate: bool,
    /// Press-to-select mode: when on, physical keypresses (observed as egui key
    /// events while the window is focused) are reverse-mapped against the
    /// loaded config to select + flash the matching button.
    listening: bool,
    /// Slot currently flashing from a physical press, with the time it started.
    pressed_key: Option<usize>,
    pressed_at: Option<Instant>,
    /// When Some((layer, key, slot)), the editor is "learning" that Basic slot:
    /// the next key press (with modifiers) captured from the Mac keyboard fills
    /// its keycode + modifiers. Cancelled by Escape or selecting elsewhere.
    learning_slot: Option<(usize, usize, usize)>,
    /// A destructive action awaiting the user's dirty-state confirmation.
    pending: Option<PendingAction>,
    /// Current LED colour selection for the layer switcher's LED picker.
    led_colour_name: String,
}

impl Default for KeyToolApp {
    fn default() -> Self {
        Self {
            worker: Worker::spawn(),
            conn: ConnState::Disconnected,
            key_count: 12,
            layer_count: 3,
            layers: empty_layers(),
            original: empty_layers(),
            selected_layer: 0,
            selected_key: 1,
            dirty: HashSet::new(),
            busy: false,
            status: "Disconnected. Click Connect.".into(),
            pending_degenerate: false,
            listening: false,
            pressed_key: None,
            pressed_at: None,
            learning_slot: None,
            pending: None,
            led_colour_name: LED_COLOURS[3].1.to_string(), // default "Green"
        }
    }
}

fn empty_layers() -> Vec<Vec<KeyRecord>> {
    (0..LAYERS)
        .map(|_| vec![KeyRecord::empty(); SLOTS + 1])
        .collect()
}

impl KeyToolApp {
    fn drain_events(&mut self) {
        while let Ok(ev) = self.worker.event_rx.try_recv() {
            match ev {
                Event::Connected(kc) => {
                    self.conn = ConnState::Connected;
                    self.key_count = if kc.keys == 0 { 12 } else { kc.keys as usize };
                    self.layer_count = if kc.layers == 0 { 3 } else { kc.layers as usize };
                    self.busy = false;
                    self.status = format!(
                        "Connected: {} keys, {} layers. Reading…",
                        self.key_count, self.layer_count
                    );
                    self.worker.send(Cmd::Read);
                    self.busy = true;
                }
                Event::Disconnected => {
                    self.conn = ConnState::Disconnected;
                    self.busy = false;
                    self.status = "Disconnected.".into();
                }
                Event::Config(read) => {
                    // A full read streams 12 keys + 9 rotary slots (25-wide
                    // rows). A degenerate-mode read stops at the 12 keys; in
                    // that case carry prior rotary state (slots 16..=24)
                    // forward so local edits survive. A full read overwrites
                    // rotaries with the device's values.
                    let read_lens: Vec<usize> = read.iter().map(|r| r.len()).collect();
                    let mut new = pad_layers(read);
                    for (l, new_layer) in new.iter_mut().enumerate().take(LAYERS) {
                        for r in protocol::ROTARY_SLOT_BASE..=SLOTS {
                            if read_lens.get(l).copied().unwrap_or(0) <= r
                                && r < new_layer.len()
                                && r < self.layers[l].len()
                            {
                                new_layer[r] = self.layers[l][r].clone();
                            }
                        }
                    }
                    self.layers = new;
                    self.original = self.layers.clone();
                    self.dirty.clear();
                    self.busy = false;
                    self.status = format!("Loaded {} layers.", self.layers.len());
                }
                Event::Written => {
                    self.dirty.clear();
                    self.original = self.layers.clone();
                    self.busy = false;
                    self.status = "Written and committed.".into();
                }
                Event::DegenerateKeyCount => {
                    self.pending_degenerate = true;
                }
                Event::Error(e) => {
                    self.busy = false;
                    if self.conn == ConnState::Connecting {
                        self.conn = ConnState::Disconnected;
                    }
                    self.status = format!("Error: {e}");
                }
            }
        }
        if self.pending_degenerate {
            self.status.push_str(
                "  (Note: the mode switch reports a degenerate key count; forced a 12-key read — config is fine.)",
            );
            self.pending_degenerate = false;
        }
    }

    fn set_record(&mut self, layer: usize, key: usize, rec: KeyRecord) {
        if layer < self.layers.len() && key < self.layers[layer].len() {
            self.layers[layer][key] = rec;
            if self.is_unchanged(layer, key) {
                self.dirty.remove(&(key as u8, layer as u8));
            } else {
                self.dirty.insert((key as u8, layer as u8));
            }
        }
    }

    fn is_unchanged(&self, layer: usize, key: usize) -> bool {
        self.original
            .get(layer)
            .and_then(|r| r.get(key))
            .map(|o| o.to_bytes() == self.layers[layer][key].to_bytes())
            .unwrap_or(true)
    }

    fn recompute_dirty(&mut self) {
        self.dirty.clear();
        for l in 0..LAYERS {
            for k in 1..=SLOTS {
                if !self.is_unchanged(l, k) {
                    self.dirty.insert((k as u8, l as u8));
                }
            }
        }
    }

    fn write_dirty(&mut self) {
        if self.dirty.is_empty() {
            self.status = "Nothing to write.".into();
            return;
        }
        let items: Vec<_> = self
            .dirty
            .iter()
            .map(|&(k, l)| (k, l, self.layers[l as usize][k as usize].clone()))
            .collect();
        self.worker.send(Cmd::Write(items));
        self.busy = true;
        self.status = "Writing…".into();
    }

    /// Restore every key to its saved (original) state.
    fn revert_all(&mut self) {
        let n = self.dirty.len();
        self.layers = self.original.clone();
        self.dirty.clear();
        self.status = if n == 0 {
            "Nothing to revert.".into()
        } else {
            format!("Reverted {n} edit(s).")
        };
    }

    /// Restore a single key to its saved (original) state.
    fn revert_key(&mut self, layer: usize, key: usize) {
        if let Some(row) = self.original.get(layer) {
            if let Some(orig) = row.get(key) {
                let rec = orig.clone();
                self.set_record(layer, key, rec);
            }
        }
    }

    /// Request an action that would discard unsaved edits. If there are dirty
    /// edits, defer it behind an inline confirm; otherwise run it now.
    fn request_action(&mut self, action: PendingAction) {
        if !self.dirty.is_empty() && self.pending.is_none() {
            self.pending = Some(action);
        } else {
            self.run_action(action);
        }
    }

    fn run_action(&mut self, action: PendingAction) {
        match action {
            PendingAction::Read => {
                self.worker.send(Cmd::Read);
                self.busy = true;
                self.status = "Reading…".into();
            }
            PendingAction::Disconnect => {
                self.worker.send(Cmd::Disconnect);
            }
            PendingAction::ClearAll => {
                for l in 0..LAYERS.min(self.layers.len()) {
                    for k in 1..=SLOTS.min(self.layers[l].len().saturating_sub(1)) {
                        self.layers[l][k] = KeyRecord::empty();
                    }
                }
                self.recompute_dirty();
                self.status = format!("Cleared all keys ({} modified).", self.dirty.len());
            }
        }
    }

    fn confirm_pending(&mut self) {
        if let Some(action) = self.pending.take() {
            self.run_action(action);
        }
    }

    /// Native save dialog → write the full config (all layers incl. rotaries)
    /// to a JSON file.
    fn do_save(&mut self) {
        let file = rfd::FileDialog::new()
            .add_filter("keytool config", &["json"])
            .set_file_name("keypad-config.json")
            .save_file();
        let Some(path) = file else { return };
        let count: usize = self.layers.iter().map(|r| r.len()).sum();
        match crate::config::save(&self.layers, &path) {
            Ok(()) => self.status = format!("Saved {} records to {}.", count, path.display()),
            Err(e) => self.status = format!("Error saving: {e}"),
        }
    }

    /// Native open dialog → load a JSON config into the editor. The loaded
    /// file becomes the new clean baseline (dirty is cleared); use Write to
    /// push it to the physical device.
    fn do_open(&mut self) {
        let file = rfd::FileDialog::new()
            .add_filter("keytool config", &["json"])
            .pick_file();
        let Some(path) = file else { return };
        match crate::config::load(&path) {
            Ok(loaded) => {
                let n = loaded.len();
                self.layers = loaded;
                self.original = self.layers.clone();
                self.dirty.clear();
                self.selected_layer = self.selected_layer.min(LAYERS.saturating_sub(1));
                self.selected_key = self.selected_key.clamp(1, SLOTS);
                self.learning_slot = None;
                self.status = format!(
                    "Loaded {n} layers from {}. Use Write to push to the device.",
                    path.display()
                );
            }
            Err(e) => self.status = format!("Error loading: {e}"),
        }
    }

    /// Fade intensity (0..1) of the press-flash for `k`, or `None` if not
    /// flashing. Used to tint the matching button green for ~300ms after a
    /// physical press is reverse-matched.
    fn flash_intensity(&self, k: usize) -> Option<f32> {
        let (Some(pk), Some(t)) = (self.pressed_key, self.pressed_at) else {
            return None;
        };
        if pk != k {
            return None;
        }
        let elapsed = t.elapsed().as_millis() as f32;
        if elapsed < 300.0 {
            Some(1.0 - elapsed / 300.0)
        } else {
            None
        }
    }

    /// Find the (layer, key) whose Basic-mode slot 0 equals `(mods, code)`.
    /// Searches all layers so a press selects the device's *active* layer
    /// (which may differ from the GUI's current layer) and switches to it.
    /// Returns the first match; duplicate mappings across layers are ambiguous.
    fn find_slot(&self, mods: u8, code: u8) -> Option<(usize, usize)> {
        for l in 0..LAYERS {
            for (k, rec) in self.layers[l].iter().enumerate().skip(1) {
                if rec.mode == Mode::Basic as u8 && rec.seq_len >= 1 {
                    let slot = rec.slots()[0];
                    // Compare left-modifier bits (the device stores LCtrl/LShift/
                    // LAlt/LGUI in the low nibble; egui only produces those).
                    if slot.b0 & 0x0F == mods & 0x0F && slot.b1 == code {
                        return Some((l, k));
                    }
                }
            }
        }
        None
    }
}

/// Normalise read layers into a fixed LAYERS×(SLOTS+1) grid (25 slots). A full
/// read supplies 12 keys + 9 rotary slots (slots 0..=24); a degenerate-mode read
/// may supply only the 12 keys, leaving the rotary slots empty — the caller
/// back-fills those from prior GUI state.
fn pad_layers(read: Vec<Vec<KeyRecord>>) -> Vec<Vec<KeyRecord>> {
    let mut out = empty_layers();
    for (l, row) in read.into_iter().enumerate().take(LAYERS) {
        for (k, rec) in row.into_iter().enumerate().take(SLOTS + 1) {
            out[l][k] = rec;
        }
    }
    out
}

impl eframe::App for KeyToolApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();

        // Expire a finished press flash.
        if let Some(t) = self.pressed_at {
            if t.elapsed() > Duration::from_millis(400) {
                self.pressed_at = None;
                self.pressed_key = None;
            }
        }

        // Learn-from-keyboard: when learning a Basic slot, capture the next
        // key press (with modifiers) and write it into that slot. Takes
        // priority over press-to-select. Escape cancels.
        if let Some((layer, key, slot_i)) = self.learning_slot {
            let learned: Option<Option<(u8, u8)>> = ctx.input(|i| {
                if i.key_pressed(egui::Key::Escape) {
                    return Some(None);
                }
                i.events.iter().find_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        let mods = mods_to_hid(modifiers);
                        egui_key_to_hid(key).map(|code| Some((mods, code)))
                    }
                    _ => None,
                }).flatten().map(Some)
            });
            if let Some(captured) = learned {
                self.learning_slot = None;
                if let Some((mods, code)) = captured {
                    let mut r = self.layers[layer][key].clone();
                    if r.mode != Mode::Basic as u8 {
                        r.mode = Mode::Basic as u8;
                    }
                    if r.seq_len == 0 {
                        r.seq_len = (slot_i + 1) as u8;
                    }
                    r.set_slot(slot_i, Slot { b0: mods, b1: code });
                    self.set_record(layer, key, r);
                }
            } else {
                ctx.request_repaint();
            }
        }

        // Press-to-select: reverse-map observed keystrokes to config slots.
        // Only fires while listening and not learning (learning consumes the
        // keypress into a slot instead). The device sends its configured
        // keystrokes (not raw button indices), so we match against the loaded
        // config across all layers — a match also switches the displayed
        // layer to the device's active one.
        if self.listening && self.learning_slot.is_none() {
            let presses: Vec<(u8, u8)> = ctx.input(|i| {
                i.events.iter().filter_map(|e| match e {
                    egui::Event::Key {
                        key,
                        pressed: true,
                        modifiers,
                        ..
                    } => {
                        let mods = mods_to_hid(modifiers);
                        egui_key_to_hid(key).map(|code| (mods, code))
                    }
                    _ => None,
                }).collect()
            });
            for (mods, code) in presses {
                if let Some((l, k)) = self.find_slot(mods, code) {
                    // Rotaries can't be reliably reverse-mapped: R1 sends a
                    // mute/volume consumer code (no Basic key to match), and
                    // R2/R3 keypress matches are unreliable. Skip rotary slots
                    // so a rotary press never highlights a key cell — select
                    // rotaries by clicking instead.
                    if is_rotary(k) {
                        continue;
                    }
                    self.selected_layer = l;
                    self.selected_key = k;
                    self.pressed_key = Some(k);
                    self.pressed_at = Some(Instant::now());
                }
            }
        }

        // ---- Top panel: connection + actions + status ---------------------
        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.add_space(2.0);
            // Row 1: connection status + device actions.
            ui.horizontal(|ui| {
                let (label, color) = match self.conn {
                    ConnState::Disconnected => ("● Disconnected", egui::Color32::from_rgb(180, 80, 80)),
                    ConnState::Connecting => ("● Connecting…", egui::Color32::from_rgb(200, 160, 40)),
                    ConnState::Connected => ("● Connected", egui::Color32::from_rgb(80, 170, 80)),
                };
                ui.colored_label(color, label);
                ui.separator();
                let can_connect = self.conn != ConnState::Connecting && !self.busy;
                ui.add_enabled_ui(can_connect, |ui| match self.conn {
                    ConnState::Disconnected | ConnState::Connecting => {
                        if ui.button("Connect").clicked() {
                            self.conn = ConnState::Connecting;
                            self.busy = true;
                            self.status = "Connecting…".into();
                            self.worker.send(Cmd::Connect);
                        }
                    }
                    ConnState::Connected => {
                        if ui.button("Disconnect").clicked() {
                            self.request_action(PendingAction::Disconnect);
                        }
                        let read_enabled = !self.busy && self.pending.is_none();
                        if ui.add_enabled(read_enabled, egui::Button::new("Read")).clicked() {
                            self.request_action(PendingAction::Read);
                        }
                        let write_enabled = !self.busy && !self.dirty.is_empty() && self.pending.is_none();
                        if ui.add_enabled(write_enabled, egui::Button::new("Write")).clicked() {
                            self.write_dirty();
                        }
                        let revert_enabled = !self.busy && !self.dirty.is_empty();
                        if ui.add_enabled(revert_enabled, egui::Button::new("Revert")).clicked() {
                            self.revert_all();
                        }
                    }
                });
                ui.separator();
                let dirty_n = self.dirty.len();
                let dirty_color = if dirty_n == 0 {
                    egui::Color32::from_gray(130)
                } else {
                    egui::Color32::from_rgb(200, 160, 40)
                };
                ui.colored_label(dirty_color, format!("{dirty_n} modified"));
            });

            // Row 2: status text + File menu + Listen toggle.
            ui.horizontal(|ui| {
                let s = self.status.clone();
                ui.label(egui::RichText::new(&s).color(status_color(&s)));
                ui.separator();
                ui.add_enabled_ui(self.conn == ConnState::Connected || self.pending.is_some(), |ui| {
                    ui.menu_button("File", |ui| {
                        if ui.button("Save…").clicked() {
                            self.do_save();
                            ui.close_menu();
                        }
                        if ui.button("Open…").clicked() {
                            self.do_open();
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui
                            .add_enabled(self.pending.is_none(), egui::Button::new("Clear all keys"))
                            .clicked()
                        {
                            self.request_action(PendingAction::ClearAll);
                            ui.close_menu();
                        }
                    });
                });
                ui.separator();
                let listen_label = if self.listening {
                    "■ Stop listening"
                } else {
                    "◎ Listen to keys"
                };
                let mut btn = egui::Button::new(listen_label);
                if self.listening {
                    btn = btn.fill(egui::Color32::from_rgb(60, 120, 60));
                }
                if ui.add(btn).clicked() {
                    self.listening = !self.listening;
                    if !self.listening {
                        self.pressed_at = None;
                        self.pressed_key = None;
                    }
                }
            });

            // Dirty warning banner + pending-action confirm.
            if self.pending.is_some() {
                ui.add_space(2.0);
                let action = self.pending.unwrap();
                ui.horizontal(|ui| {
                    ui.colored_label(
                        egui::Color32::from_rgb(200, 160, 40),
                        format!(
                            "⚠ You have {} unsaved edit(s). {} and lose them?",
                            self.dirty.len(),
                            action.label()
                        ),
                    );
                    if ui.button("Confirm").clicked() {
                        self.confirm_pending();
                    }
                    if ui.button("Cancel").clicked() {
                        self.pending = None;
                    }
                });
            } else if !self.dirty.is_empty() {
                ui.add_space(2.0);
                ui.colored_label(
                    egui::Color32::from_rgb(200, 160, 40),
                    format!("⚠ {} unsaved edit(s) — press Write to commit, or Revert to discard.", self.dirty.len()),
                );
            }
            ui.add_space(2.0);
        });

        // ---- Right side panel: the key editor ------------------------------
        egui::SidePanel::right("editor")
            .resizable(true)
            .default_width(360.0)
            .min_width(300.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.add_space(4.0);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        ui.set_min_width(ui.available_width() - 4.0);
                        self.editor(ui);
                    });
                });
            });

        // ---- Central panel: layer switcher + keypad + rotaries -------------
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(4.0);
            // Layer switcher + LED colour picker.
            ui.horizontal(|ui| {
                ui.strong("Layer");
                ui.separator();
                for l in 0..LAYERS {
                    if ui
                        .selectable_label(self.selected_layer == l, format!("Layer {}", l + 1))
                        .clicked()
                    {
                        self.selected_layer = l;
                        self.selected_key = self.selected_key.clamp(1, SLOTS);
                        self.learning_slot = None;
                    }
                }
                ui.separator();
                // Per-layer LED colour picker (capture-confirmed format).
                ui.label("LED colour:");
                let selected_text = LED_COLOURS
                    .iter()
                    .find(|(_, n)| **n == self.led_colour_name)
                    .map(|(_, n)| *n)
                    .unwrap_or("—");
                egui::ComboBox::from_id_salt("layer_led_colour")
                    .selected_text(selected_text)
                    .show_ui(ui, |ui| {
                        for (idx, name) in LED_COLOURS {
                            if ui.selectable_label(self.led_colour_name == *name, *name).clicked() {
                                self.led_colour_name = (*name).to_string();
                                if self.conn == ConnState::Connected && !self.busy {
                                    self.worker.send(Cmd::WriteLed(
                                        self.selected_layer as u8 + 1, // device layer 1-indexed
                                        *idx as u8,
                                    ));
                                }
                            }
                        }
                    });
            });
            ui.add_space(8.0);

            // Keypad: 4-wide × 3-tall grid (90°-rotated layout).
            ui.label("Keys (click to edit):");
            egui::Grid::new("key_grid")
                .num_columns(4)
                .spacing([8.0, 8.0])
                .show(ui, |ui| {
                    for (i, &k) in PHYSICAL_ORDER.iter().enumerate() {
                        self.key_button(ui, k, false);
                        if i % 4 == 3 {
                            ui.end_row();
                        }
                    }
                });

            ui.add_space(10.0);
            ui.label("Rotaries (click a direction to edit):");
            // Three knobs side by side; each shows its three direction slots
            // (turn-left / press / turn-right) stacked vertically.
            ui.horizontal(|ui| {
                for knob in 1..=protocol::ROTARY_COUNT {
                    ui.vertical(|ui| {
                        ui.strong(format!("Knob {knob}"));
                        for dir in RotaryDir::ALL {
                            let k = protocol::rotary_slot(knob, dir);
                            self.key_button(ui, k, true);
                        }
                    });
                    ui.add_space(18.0);
                }
            });
        });

        if self.busy || self.pressed_at.is_some() || self.learning_slot.is_some() || self.pending.is_some()
        {
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.worker.stop();
    }
}

impl KeyToolApp {
    /// Render one keypad button (key or rotary). Shared styling: dirty
    /// highlight, selection stroke, press-flash green tint, "•" dirty prefix.
    fn key_button(&mut self, ui: &mut egui::Ui, k: usize, rotary: bool) {
        let rec = self.layers[self.selected_layer][k].clone();
        let is_dirty = self.dirty.contains(&(k as u8, self.selected_layer as u8));
        let is_sel = self.selected_key == k;
        let label = if is_dirty {
            format!("• {}\n{}", key_label(&rec, rotary, k), short_label(&rec))
        } else {
            format!("{}\n{}", key_label(&rec, rotary, k), short_label(&rec))
        };
        let mut btn = egui::Button::new(label).wrap().min_size(egui::vec2(78.0, 52.0));
        if is_sel {
            let stroke_color = if rotary {
                egui::Color32::from_rgb(120, 180, 255)
            } else {
                egui::Color32::WHITE
            };
            btn = btn.stroke(egui::Stroke::new(2.0, stroke_color));
        } else if rotary {
            btn = btn.stroke(egui::Stroke::new(1.0, egui::Color32::from_rgb(70, 70, 80)));
        }
        if let Some(f) = self.flash_intensity(k) {
            btn = btn.fill(egui::Color32::from_rgba_unmultiplied(
                100, 220, 100, (190.0 * f) as u8,
            ));
        }
        let resp = ui.add(btn);
        let resp = if is_dirty { resp.highlight() } else { resp };
        if resp.clicked() {
            self.selected_key = k;
            self.learning_slot = None;
        }
    }

    fn editor(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let what = if let Some((knob, dir)) = protocol::rotary_slot_parse(self.selected_key) {
                format!("Knob {knob} {} (slot {})", dir.name(), self.selected_key)
            } else {
                format!("Key {} (slot {})", self.selected_key, self.selected_key)
            };
            ui.heading(format!("{what} · Layer {}", self.selected_layer + 1));
        });
        if self.learning_slot.is_some() {
            ui.add_space(2.0);
            ui.colored_label(
                egui::Color32::from_rgb(120, 160, 255),
                "Learning — press a key (with modifiers) to assign it. Esc to cancel. (The Win/Cmd modifier can't be captured on macOS.)",
            );
        }
        ui.add_space(4.0);

        let layer = self.selected_layer;
        let key = self.selected_key;
        let rec = self.layers[layer][key].clone();

        // Mode selector.
        ui.horizontal(|ui| {
            ui.label("Mode:");
            let modes: [(u8, &str); 5] = [
                (1, "Basic"),
                (2, "Multimedia"),
                (3, "Mouse"),
                (5, "LED layer-indicator"),
                (8, "Macro"),
            ];
            let current_name = mode_name(rec.mode);
            egui::ComboBox::from_id_salt("mode_select")
                .selected_text(current_name)
                .show_ui(ui, |ui| {
                    for (m, name) in modes {
                        if ui.selectable_label(rec.mode == m, name).clicked() {
                            let mut r = rec.clone();
                            r.mode = m;
                            if m == 1 && r.seq_len == 0 {
                                r.seq_len = 1;
                                r.set_slot(0, Slot::default());
                            }
                            if m == 5 {
                                r.raw[0] = 0xFE;
                            }
                            self.set_record(layer, key, r);
                        }
                    }
                });
        });

        ui.add_space(4.0);
        match rec.mode_enum() {
            Some(Mode::Basic) => self.edit_basic(ui, layer, key, &rec),
            Some(Mode::Multimedia) => self.edit_multimedia(ui, layer, key, &rec),
            Some(Mode::Mouse) => self.edit_mouse(ui, layer, key, &rec),
            Some(Mode::Led) => self.edit_led(ui, layer, key, &rec),
            Some(Mode::Macro) => {
                ui.label("Macro mode — payload stored opaquely (edit raw bytes below).");
                self.edit_raw(ui, layer, key, &rec);
            }
            None => {
                ui.label(format!("Unknown mode {}. Edit raw bytes:", rec.mode));
                self.edit_raw(ui, layer, key, &rec);
            }
        }

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label("Delay:");
            let mut delay = rec.delay;
            if ui
                .add(egui::DragValue::new(&mut delay).range(0..=255))
                .changed()
            {
                let mut r = rec.clone();
                r.delay = delay;
                self.set_record(layer, key, r);
            }
            ui.separator();
            let is_dirty = self.dirty.contains(&(key as u8, layer as u8));
            if ui.add_enabled(is_dirty, egui::Button::new("Revert to saved")).clicked() {
                self.revert_key(layer, key);
            }
            if ui.button("Clear key").clicked() {
                self.set_record(layer, key, KeyRecord::empty());
            }
        });
    }

    fn edit_basic(&mut self, ui: &mut egui::Ui, layer: usize, key: usize, rec: &KeyRecord) {
        ui.horizontal(|ui| {
            ui.label("Sequence length (slots):");
            let mut len = rec.seq_len;
            if ui
                .add(egui::DragValue::new(&mut len).range(0..=protocol::SLOTS as u32))
                .changed()
            {
                let mut r = rec.clone();
                r.seq_len = len;
                self.set_record(layer, key, r);
            }
        });
        ui.add_space(4.0);
        egui::Grid::new("slots").num_columns(5).show(ui, |ui| {
            for i in 0..protocol::SLOTS {
                if i >= rec.seq_len as usize {
                    ui.add_enabled(false, egui::Label::new(format!("slot {i}: (unused)")));
                    ui.end_row();
                    continue;
                }
                let slot = rec.slots()[i];
                ui.label(format!("slot {i}:"));
                let mods = slot.b0;
                let mut new_mods = mods;
                let bits: &[(u8, &str)] = &[
                    (hid_codes::MOD_LCTRL, "Ctrl"),
                    (hid_codes::MOD_LSHIFT, "Shift"),
                    (hid_codes::MOD_LALT, "Alt"),
                    (hid_codes::MOD_LGUI, "Win"),
                ];
                for (bit, name) in bits {
                    let mut on = mods & bit != 0;
                    if ui.checkbox(&mut on, *name).changed() {
                        if on {
                            new_mods |= bit;
                        } else {
                            new_mods &= !bit;
                        }
                    }
                }
                // Key picker (named categories) + live name.
                egui::ComboBox::from_id_salt(format!("pick_{key}_{i}"))
                    .selected_text(hid_codes::key_name(slot.b1))
                    .show_ui(ui, |ui| {
                        for (group, entries) in hid_codes::key_presets() {
                            ui.label(*group);
                            for (code, name) in *entries {
                                if ui.selectable_label(slot.b1 == *code, *name).clicked() {
                                    let mut r = rec.clone();
                                    r.set_slot(i, Slot { b0: slot.b0, b1: *code });
                                    self.set_record(layer, key, r);
                                }
                            }
                            ui.separator();
                        }
                    });
                ui.label(format!("→ {}", hid_codes::key_name(slot.b1)));
                // Learn-from-keyboard: capture the next key press into this slot.
                let learning = self.learning_slot == Some((layer, key, i));
                let label = if learning {
                    "◖ press a key…"
                } else {
                    "Learn"
                };
                let mut btn = egui::Button::new(label);
                if learning {
                    btn = btn.fill(egui::Color32::from_rgb(80, 100, 160));
                }
                if ui.add(btn).clicked() {
                    self.learning_slot = if learning { None } else { Some((layer, key, i)) };
                }
                if new_mods != slot.b0 {
                    let mut r = rec.clone();
                    r.set_slot(i, Slot { b0: new_mods, b1: slot.b1 });
                    self.set_record(layer, key, r);
                }
                ui.end_row();
            }
        });

        ui.add_space(4.0);
        ui.collapsing("Advanced (raw key code)", |ui| {
            let slot = rec.slots()[0];
            let mut code = slot.b1;
            let resp = ui.add(
                egui::DragValue::new(&mut code)
                    .range(0..=0xE7)
                    .speed(0.1)
                    .prefix("slot 0 key: "),
            );
            if resp.changed() {
                let mut r = rec.clone();
                r.set_slot(0, Slot { b0: slot.b0, b1: code });
                self.set_record(layer, key, r);
            }
        });
    }

    fn edit_multimedia(&mut self, ui: &mut egui::Ui, layer: usize, key: usize, rec: &KeyRecord) {
        ui.label("Multimedia: pick a media key (consumer code → slot 0 byte 0).");
        let slot = rec.slots()[0];
        ui.horizontal(|ui| {
            ui.label("Media key:");
            egui::ComboBox::from_id_salt(format!("media_{key}"))
                .selected_text(hid_codes::consumer_name(slot.b0))
                .show_ui(ui, |ui| {
                    for (code, name) in hid_codes::consumer_presets() {
                        if ui.selectable_label(slot.b0 == *code, *name).clicked() {
                            let mut r = rec.clone();
                            r.seq_len = 2;
                            r.set_slot(0, Slot { b0: *code, b1: slot.b1 });
                            self.set_record(layer, key, r);
                        }
                    }
                });
        });
        ui.add_space(4.0);
        ui.collapsing("Advanced (raw bytes)", |ui| {
            ui.horizontal(|ui| {
                ui.label("Consumer code:");
                let mut b0 = slot.b0;
                let r0 = ui.add(egui::DragValue::new(&mut b0).range(0..=255));
                ui.label("Page/aux:");
                let mut b1 = slot.b1;
                let r1 = ui.add(egui::DragValue::new(&mut b1).range(0..=255));
                if r0.changed() || r1.changed() {
                    let mut r = rec.clone();
                    r.seq_len = 2;
                    r.set_slot(0, Slot { b0, b1 });
                    self.set_record(layer, key, r);
                }
            });
        });
    }

    fn edit_mouse(&mut self, ui: &mut egui::Ui, layer: usize, key: usize, rec: &KeyRecord) {
        ui.label("Mouse action.");
        let mut b = rec.raw[protocol::off::MOUSE_BTN];
        let mut s = rec.raw[protocol::off::SUBTYPE];
        let mut p = rec.raw[protocol::off::MOUSE_PARAM];
        ui.horizontal(|ui| {
            ui.label("Button:");
            egui::ComboBox::from_id_salt(format!("mouse_btn_{key}"))
                .selected_text(mouse_btn_name(b))
                .show_ui(ui, |ui| {
                    for (val, name) in [(1u8, "Left"), (2, "Right"), (4, "Middle")] {
                        if ui.selectable_label(b == val, name).clicked() {
                            b = val;
                        }
                    }
                });
        });
        ui.collapsing("Advanced (raw bytes)", |ui| {
            ui.horizontal(|ui| {
                ui.label("Button/action:");
                ui.add(egui::DragValue::new(&mut b).range(0..=255));
                ui.label("Subtype:");
                ui.add(egui::DragValue::new(&mut s).range(0..=255));
                ui.label("Param:");
                ui.add(egui::DragValue::new(&mut p).range(0..=255));
            });
        });
        // Commit once if the combo or any advanced field changed. (The
        // collapsing body only runs when open, but `b/s/p` keep their rec
        // values when it's closed, so `changed` stays false for closed edits.)
        let changed = b != rec.raw[protocol::off::MOUSE_BTN]
            || s != rec.raw[protocol::off::SUBTYPE]
            || p != rec.raw[protocol::off::MOUSE_PARAM];
        if changed {
            let mut r = rec.clone();
            r.mode = Mode::Mouse as u8;
            r.raw[protocol::off::MOUSE_BTN] = b;
            r.raw[protocol::off::SUBTYPE] = s;
            r.raw[protocol::off::MOUSE_PARAM] = p;
            r.raw[protocol::off::SEQ_LEN] = 1;
            r.raw[protocol::off::MODE] = Mode::Mouse as u8;
            self.set_record(layer, key, r);
        }
    }

    /// LED layer-indicator editor: now unparked and functional. The LED write uses
    /// the reserved-slot path (record[1]=0xB0) via `Cmd::WriteLed` on the worker
    /// thread — independent of the key-config dirty state. Shows the current
    /// bitmask from the device and lets you pick a colour (1..7) per layer.
    fn edit_led(&mut self, ui: &mut egui::Ui, _layer: usize, _key: usize, rec: &KeyRecord) {
        let current_bitmask = rec.raw[protocol::off::LED_BITMASK];
        let colour_idx = (current_bitmask >> 4) & 0x0F; // high nibble = group
        let key_bits = current_bitmask & 0x0F;           // low nibble = key bitmask

        ui.label("LED layer-indicator colour (writes directly to device):");
        ui.add_space(2.0);

        egui::ComboBox::from_id_salt("led_colour_picker")
            .selected_text(
                LED_COLOURS
                    .iter()
                    .find(|(i, _)| *i == colour_idx as usize)
                    .map(|(_, n)| *n)
                    .unwrap_or("—"),
            )
            .show_ui(ui, |ui| {
                for (idx, name) in LED_COLOURS {
                    if ui.selectable_label(colour_idx as usize == *idx, *name).clicked() {
                        if self.conn == ConnState::Connected && !self.busy {
                            self.worker.send(Cmd::WriteLed(
                                self.selected_layer as u8 + 1, // device layer 1-indexed
                                *idx as u8,
                            ));
                        }
                    }
                }
            });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        ui.label("Current bitmask (read from device):");
        ui.horizontal(|ui| {
            ui.monospace(format!("0x{:02X}", current_bitmask));
            ui.label(format!(
                " → colour group {} ({}), key bits 0x{:X}",
                colour_idx,
                LED_COLOURS
                    .iter()
                    .find(|(i, _)| *i == colour_idx as usize)
                    .map(|(_, n)| *n)
                    .unwrap_or("?"),
                key_bits
            ));
        });

        ui.add_space(4.0);
        ui.separator();
        ui.add_space(4.0);

        ui.label("Full record (raw bytes):");
        let mut buf = String::new();
        for byte in rec.to_bytes().iter() {
            buf.push_str(&format!("{byte:02x} "));
        }
        ui.add(egui::TextEdit::multiline(&mut buf).desired_width(f32::INFINITY));
    }

    fn edit_raw(&mut self, ui: &mut egui::Ui, layer: usize, key: usize, rec: &KeyRecord) {
        ui.label("Raw 50-byte record (hex pairs, space-separated):");
        let mut buf = String::new();
        for b in rec.to_bytes().iter() {
            buf.push_str(&format!("{b:02x} "));
        }
        let resp =
            egui::TextEdit::multiline(&mut buf).desired_width(f32::INFINITY).show(ui).response;
        if resp.changed() {
            let bytes: Vec<u8> = buf
                .split_whitespace()
                .filter_map(|s| u8::from_str_radix(s, 16).ok())
                .collect();
            if bytes.len() == protocol::RECORD_LEN {
                let mut arr = [0u8; protocol::RECORD_LEN];
                arr.copy_from_slice(&bytes);
                self.set_record(layer, key, KeyRecord::from_bytes(&arr));
            }
        }
    }
}

/// Short single-line label for a key button.
fn short_label(rec: &KeyRecord) -> String {
    let s = label_for(rec);
    s.split('\n').next().unwrap_or(&s).to_string()
}

/// Header label (key number / rotary knob + direction) for a button — kept
/// separate so the dirty "•" marker can prefix it cleanly.
fn key_label(_rec: &KeyRecord, rotary: bool, k: usize) -> String {
    if rotary {
        if let Some((knob, dir)) = protocol::rotary_slot_parse(k) {
            let sym = match dir {
                RotaryDir::Left => "◀",
                RotaryDir::Press => "●",
                RotaryDir::Right => "▶",
            };
            format!("R{knob} {sym}")
        } else {
            format!("slot {k}")
        }
    } else {
        format!("{k}")
    }
}

/// Display name for a mode byte (covers the known modes + a fallback).
fn mode_name(m: u8) -> &'static str {
    match Mode::from_byte(m) {
        Some(Mode::Basic) => "Basic",
        Some(Mode::Multimedia) => "Multimedia",
        Some(Mode::Mouse) => "Mouse",
        Some(Mode::Led) => "LED (parked)",
        Some(Mode::Macro) => "Macro",
        None => "Unknown",
    }
}

/// Name for a mouse button byte.
fn mouse_btn_name(b: u8) -> &'static str {
    match b {
        1 => "Left",
        2 => "Right",
        4 => "Middle",
        _ => "(custom)",
    }
}

/// Color for the status line: red on errors, neutral otherwise.
fn status_color(s: &str) -> egui::Color32 {
    if s.to_ascii_lowercase().starts_with("error") {
        egui::Color32::from_rgb(220, 80, 80)
    } else {
        egui::Color32::from_gray(200)
    }
}

/// Convert egui modifier state to a HID modifier byte (low nibble only:
/// LCtrl/LShift/LAlt/LGUI), matching how the device stores Basic-mode mods.
fn mods_to_hid(m: &egui::Modifiers) -> u8 {
    let mut b = 0u8;
    if m.ctrl {
        b |= 0x01;
    }
    if m.shift {
        b |= 0x02;
    }
    if m.alt {
        b |= 0x04;
    }
    if m.mac_cmd {
        b |= 0x08;
    }
    b
}

/// Map an egui key to its USB HID keyboard usage code, for reverse-matching
/// against Basic-mode config. Covers letters, F1–F12 and common control keys —
/// enough for the keypad's typical assignments. Digits/symbols arrive via
/// text events and aren't matched here.
fn egui_key_to_hid(key: &egui::Key) -> Option<u8> {
    use egui::Key;
    let code = match key {
        Key::A => 0x04, Key::B => 0x05, Key::C => 0x06, Key::D => 0x07, Key::E => 0x08,
        Key::F => 0x09, Key::G => 0x0A, Key::H => 0x0B, Key::I => 0x0C, Key::J => 0x0D,
        Key::K => 0x0E, Key::L => 0x0F, Key::M => 0x10, Key::N => 0x11, Key::O => 0x12,
        Key::P => 0x13, Key::Q => 0x14, Key::R => 0x15, Key::S => 0x16, Key::T => 0x17,
        Key::U => 0x18, Key::V => 0x19, Key::W => 0x1A, Key::X => 0x1B, Key::Y => 0x1C,
        Key::Z => 0x1D,
        Key::F1 => 0x3A, Key::F2 => 0x3B, Key::F3 => 0x3C, Key::F4 => 0x3D,
        Key::F5 => 0x3E, Key::F6 => 0x3F, Key::F7 => 0x40, Key::F8 => 0x41,
        Key::F9 => 0x42, Key::F10 => 0x43, Key::F11 => 0x44, Key::F12 => 0x45,
        Key::Enter => 0x28, Key::Escape => 0x29, Key::Backspace => 0x2A,
        Key::Tab => 0x2B, Key::Space => 0x2C,
        Key::ArrowUp => 0x52, Key::ArrowDown => 0x51,
        Key::ArrowLeft => 0x50, Key::ArrowRight => 0x4F,
        Key::Insert => 0x49, Key::Delete => 0x4C, Key::Home => 0x4A, Key::End => 0x4D,
        Key::PageUp => 0x4B, Key::PageDown => 0x4E,
        _ => return None,
    };
    Some(code)
}

pub fn run() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 640.0])
            .with_min_inner_size([760.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native(
        "keytool — Macro Keypad",
        options,
        Box::new(|_cc| Ok(Box::new(KeyToolApp::default()))),
    )
}
