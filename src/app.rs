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
    /// Layer-indicator LED colour index (1..=7) per layer, as last set from
    /// this app. `None` = unknown: the keypad's read stream doesn't include the
    /// slot-0 LED header, so the colour can't be read back from the device
    /// (see `src/bin/ledread.rs`). Persisted between runs via eframe storage.
    led_colours: [Option<usize>; LAYERS],
    /// `(layer, colour)` of a `Cmd::WriteLed` in flight. The worker answers both
    /// key writes and LED writes with `Event::Written`; this tells them apart so
    /// an LED change doesn't mark unwritten key edits as saved.
    led_write_pending: Option<(usize, usize)>,
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
            status: "Not connected.".into(),
            pending_degenerate: false,
            listening: false,
            pressed_key: None,
            pressed_at: None,
            learning_slot: None,
            pending: None,
            led_colours: [None; LAYERS],
            led_write_pending: None,
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
                Event::Written if self.led_write_pending.is_some() => {
                    if let Some((layer, colour)) = self.led_write_pending.take() {
                        self.led_colours[layer] = Some(colour);
                        self.status = format!(
                            "Layer {} LED set to {}.",
                            layer + 1,
                            led_name(colour).to_lowercase()
                        );
                    }
                    self.busy = false;
                }
                Event::Written => {
                    self.dirty.clear();
                    self.original = self.layers.clone();
                    self.busy = false;
                    self.status = "Saved to the keypad.".into();
                }
                Event::DegenerateKeyCount => {
                    self.pending_degenerate = true;
                }
                Event::Error(e) => {
                    self.busy = false;
                    self.led_write_pending = None;
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

    fn connect(&mut self) {
        self.conn = ConnState::Connecting;
        self.busy = true;
        self.status = "Connecting…".into();
        self.worker.send(Cmd::Connect);
    }

    /// Send the layer-indicator LED colour (1..=7) for the selected layer.
    fn write_led(&mut self, colour_idx: usize) {
        if self.conn == ConnState::Connected && !self.busy {
            self.worker.send(Cmd::WriteLed(
                self.selected_layer as u8 + 1, // device layer 1-indexed
                colour_idx as u8,
            ));
            self.led_write_pending = Some((self.selected_layer, colour_idx));
            self.busy = true;
            self.status = "Setting LED colour…".into();
        }
    }

    /// True once any slot holds an assignment (read from the device or a file).
    fn has_data(&self) -> bool {
        self.layers.iter().flatten().any(|r| r.mode != 0)
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

/// eframe storage key for the remembered per-layer LED colours.
const LED_COLOURS_KEY: &str = "led_colours";

impl eframe::App for KeyToolApp {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, LED_COLOURS_KEY, &self.led_colours);
    }

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

        let pal = Palette::of_ctx(ctx);

        // ---- Header: title, connection, device actions, file + listen ------
        egui::TopBottomPanel::top("top")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(14.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("Macro Keypad").size(17.0).strong());
                    ui.add_space(6.0);
                    self.connection_pill(ui, &pal);
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(2.0);
                    self.device_buttons(ui, &pal);

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let listen = ui
                            .add(egui::Button::new("Listen for keys").selected(self.listening))
                            .on_hover_text(
                                "Press keys on the keypad to jump to them here.\n\
                                 Matches the keystroke each key sends, so the window must be focused.",
                            );
                        if listen.clicked() {
                            self.listening = !self.listening;
                            if !self.listening {
                                self.pressed_at = None;
                                self.pressed_key = None;
                            }
                        }
                        self.file_menu(ui);
                    });
                });

                if let Some(action) = self.pending {
                    ui.add_space(8.0);
                    egui::Frame::none()
                        .fill(pal.warn_fill)
                        .stroke(egui::Stroke::new(1.0, pal.warn))
                        .rounding(6.0)
                        .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new("⚠").color(pal.warn).strong());
                                ui.label(format!(
                                    "You have {}. {} and discard them?",
                                    plural(self.dirty.len(), "unsaved change"),
                                    capitalise(action.label()),
                                ));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui.button("Cancel").clicked() {
                                            self.pending = None;
                                        }
                                        let discard = egui::Button::new(
                                            egui::RichText::new("Discard changes")
                                                .color(egui::Color32::WHITE),
                                        )
                                        .fill(pal.danger);
                                        if ui.add(discard).clicked() {
                                            self.confirm_pending();
                                        }
                                    },
                                );
                            });
                        });
                }
            });

        // ---- Status bar ---------------------------------------------------
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(14.0, 6.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if self.busy {
                        ui.spinner();
                    }
                    let is_error = self.status.to_ascii_lowercase().starts_with("error");
                    let color = if is_error { pal.danger } else { pal.weak };
                    ui.add(egui::Label::new(egui::RichText::new(&self.status).color(color)).truncate());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let n = self.dirty.len();
                        if n == 0 {
                            ui.label(egui::RichText::new("No unsaved changes").color(pal.faint));
                        } else {
                            ui.label(
                                egui::RichText::new(format!("{} not written", plural(n, "change")))
                                    .color(pal.warn),
                            );
                        }
                        if self.listening {
                            ui.separator();
                            ui.label(egui::RichText::new("Listening for keypresses").color(pal.accent));
                        }
                    });
                });
            });

        // ---- Right side panel: the key editor ------------------------------
        egui::SidePanel::right("editor")
            .resizable(true)
            .default_width(390.0)
            .min_width(340.0)
            .frame(
                egui::Frame::side_top_panel(&ctx.style()).inner_margin(egui::Margin::same(16.0)),
            )
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    self.editor(ui, &pal);
                });
            });

        // ---- Central panel: layer switcher + keypad + knobs ----------------
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(&ctx.style()).inner_margin(egui::Margin::same(18.0)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                    if self.conn == ConnState::Disconnected && !self.has_data() {
                        self.welcome(ui, &pal);
                        ui.add_space(16.0);
                    }
                    self.layer_bar(ui, &pal);
                    ui.add_space(18.0);

                    const GAP: f32 = 10.0;
                    let avail = ui.available_width();
                    let w = ((avail - 3.0 * GAP) / 4.0).clamp(96.0, 168.0);

                    section_label(ui, &pal, "Keys");
                    for row in PHYSICAL_ORDER.chunks(4) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = GAP;
                            for &k in row {
                                self.key_card(ui, k, egui::vec2(w, 80.0));
                            }
                        });
                        ui.add_space(GAP - ui.spacing().item_spacing.y);
                    }

                    ui.add_space(14.0);
                    section_label(ui, &pal, "Knobs");
                    let knob_w = (4.0 * w + GAP) / 3.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        for knob in 1..=protocol::ROTARY_COUNT {
                            ui.vertical(|ui| {
                                ui.set_width(knob_w);
                                ui.label(egui::RichText::new(format!("Knob {knob}")).strong());
                                for dir in RotaryDir::ALL {
                                    let k = protocol::rotary_slot(knob, dir);
                                    self.key_card(ui, k, egui::vec2(knob_w, 70.0));
                                }
                            });
                        }
                    });
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

// ---- Header pieces --------------------------------------------------------

impl KeyToolApp {
    fn connection_pill(&self, ui: &mut egui::Ui, pal: &Palette) {
        let (text, color) = match self.conn {
            ConnState::Disconnected => ("Not connected", pal.faint),
            ConnState::Connecting => ("Connecting…", pal.warn),
            ConnState::Connected => ("Connected", pal.ok),
        };
        egui::Frame::none()
            .fill(color.gamma_multiply(0.14))
            .rounding(12.0)
            .inner_margin(egui::Margin::symmetric(10.0, 3.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    dot(ui, color, 4.0);
                    let label = if self.conn == ConnState::Connected {
                        format!("{text} · {} keys, {} layers", self.key_count, self.layer_count)
                    } else {
                        text.to_string()
                    };
                    ui.label(egui::RichText::new(label).color(color).size(12.5));
                });
            });
    }

    fn device_buttons(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let idle = !self.busy && self.pending.is_none();
        match self.conn {
            ConnState::Disconnected | ConnState::Connecting => {
                let btn = egui::Button::new(
                    egui::RichText::new("Connect").color(egui::Color32::WHITE),
                )
                .fill(pal.accent);
                if ui
                    .add_enabled(idle && self.conn == ConnState::Disconnected, btn)
                    .on_hover_text("Find the keypad over USB and read its configuration")
                    .clicked()
                {
                    self.connect();
                }
            }
            ConnState::Connected => {
                let n = self.dirty.len();
                let write_label = if n == 0 {
                    "Write to keypad".to_string()
                } else {
                    format!("Write {}", plural(n, "change"))
                };
                let mut write = egui::Button::new(if n > 0 {
                    egui::RichText::new(write_label).color(egui::Color32::WHITE)
                } else {
                    egui::RichText::new(write_label)
                });
                if n > 0 {
                    write = write.fill(pal.accent);
                }
                if ui
                    .add_enabled(idle && n > 0, write)
                    .on_hover_text("Send your changes to the keypad and save them on it")
                    .clicked()
                {
                    self.write_dirty();
                }
                if ui
                    .add_enabled(!self.busy && n > 0, egui::Button::new("Revert"))
                    .on_hover_text("Discard all unsaved changes")
                    .clicked()
                {
                    self.revert_all();
                }
                if ui
                    .add_enabled(idle, egui::Button::new("Reload"))
                    .on_hover_text("Read the configuration from the keypad again")
                    .clicked()
                {
                    self.request_action(PendingAction::Read);
                }
                if ui
                    .add_enabled(!self.busy && self.pending.is_none(), egui::Button::new("Disconnect"))
                    .clicked()
                {
                    self.request_action(PendingAction::Disconnect);
                }
            }
        }
    }

    fn file_menu(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled_ui(self.pending.is_none(), |ui| {
            ui.menu_button("File", |ui| {
                ui.set_min_width(180.0);
                if ui.button("Open config…").clicked() {
                    ui.close_menu();
                    self.do_open();
                }
                if ui.button("Save config as…").clicked() {
                    ui.close_menu();
                    self.do_save();
                }
                ui.separator();
                if ui.button("Clear all keys").clicked() {
                    ui.close_menu();
                    self.request_action(PendingAction::ClearAll);
                }
            });
        });
    }

    fn welcome(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        card_frame(pal).inner_margin(egui::Margin::same(16.0)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("No keypad connected").strong().size(15.0));
            ui.add_space(2.0);
            ui.label(
                egui::RichText::new(
                    "Plug in the keypad and connect to read its current setup, \
                     or open a saved config to edit it offline.",
                )
                .color(pal.weak),
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let btn = egui::Button::new(egui::RichText::new("Connect").color(egui::Color32::WHITE))
                    .fill(pal.accent);
                if ui.add_enabled(!self.busy, btn).clicked() {
                    self.connect();
                }
                if ui.button("Open config…").clicked() {
                    self.do_open();
                }
            });
        });
    }

    fn layer_bar(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        ui.horizontal(|ui| {
            // Segmented layer switcher.
            card_frame(pal)
                .inner_margin(egui::Margin::same(3.0))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 2.0;
                        ui.spacing_mut().button_padding = egui::vec2(14.0, 5.0);
                        for l in 0..LAYERS {
                            let edits = self.dirty.iter().filter(|&&(_, dl)| dl as usize == l).count();
                            let text = if edits > 0 {
                                format!("Layer {}  •", l + 1)
                            } else {
                                format!("Layer {}", l + 1)
                            };
                            if ui.selectable_label(self.selected_layer == l, text).clicked() {
                                self.selected_layer = l;
                                self.selected_key = self.selected_key.clamp(1, SLOTS);
                                self.learning_slot = None;
                            }
                        }
                    });
                });

            ui.add_space(12.0);
            // Per-layer LED indicator colour (capture-confirmed format).
            ui.label(egui::RichText::new("Layer LED").color(pal.weak));
            let current = self.led_colours[self.selected_layer];
            if let Some(idx) = current {
                dot(ui, led_rgb(idx), 6.0);
            }
            let can_write = self.conn == ConnState::Connected && !self.busy;
            ui.add_enabled_ui(can_write, |ui| {
                egui::ComboBox::from_id_salt("layer_led_colour")
                    .width(96.0)
                    .selected_text(match current {
                        Some(idx) => egui::RichText::new(led_name(idx)),
                        None => egui::RichText::new("Unknown").color(pal.faint),
                    })
                    .show_ui(ui, |ui| {
                        for &(idx, name) in LED_COLOURS {
                            let picked = ui
                                .horizontal(|ui| {
                                    dot(ui, led_rgb(idx), 5.0);
                                    ui.selectable_label(current == Some(idx), name)
                                })
                                .inner;
                            if picked.clicked() {
                                self.write_led(idx);
                            }
                        }
                    })
                    .response
                    .on_hover_text(
                        "Colour the keypad flashes when you switch to this layer.\n\
                         The keypad can't report this colour, so this is the colour\n\
                         last set from this app (\"Unknown\" if never set here).",
                    )
                    .on_disabled_hover_text("Connect the keypad to change the LED colour");
            });
        });
    }

    /// Render one keypad cell (key or rotary direction) as a card.
    fn key_card(&mut self, ui: &mut egui::Ui, k: usize, size: egui::Vec2) {
        let rec = &self.layers[self.selected_layer][k];
        let corner = match protocol::rotary_slot_parse(k) {
            Some((_, RotaryDir::Left)) => "◀  Turn left".to_string(),
            Some((_, RotaryDir::Press)) => "Press".to_string(),
            Some((_, RotaryDir::Right)) => "Turn right  ▶".to_string(),
            None => format!("{k}"),
        };
        let card = Card {
            corner,
            title: action_title(rec),
            tag: mode_tag(rec.mode),
            dirty: self.dirty.contains(&(k as u8, self.selected_layer as u8)),
            selected: self.selected_key == k,
            flash: self.flash_intensity(k),
            max_rows: if is_rotary(k) { 1 } else { 2 },
        };
        let tooltip = if rec.mode == 0 {
            "Unassigned — click to set an action".to_string()
        } else {
            format!("{}: {}", mode_name(rec.mode), label_for(rec))
        };
        let resp = paint_card(ui, size, &card)
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(tooltip);
        if resp.clicked() {
            self.selected_key = k;
            self.learning_slot = None;
        }
    }
}

// ---- Editor panel ---------------------------------------------------------

impl KeyToolApp {
    fn editor(&mut self, ui: &mut egui::Ui, pal: &Palette) {
        let layer = self.selected_layer;
        let key = self.selected_key;
        let rec = self.layers[layer][key].clone();
        let is_dirty = self.dirty.contains(&(key as u8, layer as u8));

        let title = match protocol::rotary_slot_parse(key) {
            Some((knob, dir)) => format!("Knob {knob} · {}", capitalise(dir.name())),
            None => format!("Key {key}"),
        };
        ui.horizontal(|ui| {
            ui.heading(egui::RichText::new(title).strong());
            if is_dirty {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    badge(ui, "Modified", pal.warn);
                });
            }
        });
        ui.label(
            egui::RichText::new(format!("Layer {} · slot {key}", layer + 1))
                .color(pal.weak)
                .size(12.0),
        );
        ui.add_space(12.0);

        // Action (mode) selector.
        section_label(ui, pal, "Action");
        let modes: [(u8, &str); 6] = [
            (0, "None"),
            (1, "Keystroke"),
            (2, "Media key"),
            (3, "Mouse"),
            (5, "LED indicator"),
            (8, "Macro"),
        ];
        egui::ComboBox::from_id_salt("mode_select")
            .width(ui.available_width().min(240.0))
            .selected_text(mode_name(rec.mode))
            .show_ui(ui, |ui| {
                for (m, name) in modes {
                    if ui.selectable_label(rec.mode == m, name).clicked() && rec.mode != m {
                        let r = if m == 0 {
                            KeyRecord::empty()
                        } else {
                            let mut r = rec.clone();
                            r.mode = m;
                            if m == 1 && r.seq_len == 0 {
                                r.seq_len = 1;
                                r.set_slot(0, Slot::default());
                            }
                            if m == 5 {
                                r.raw[0] = 0xFE;
                            }
                            r
                        };
                        self.learning_slot = None;
                        self.set_record(layer, key, r);
                    }
                }
            });

        ui.add_space(14.0);
        match rec.mode_enum() {
            Some(Mode::Basic) => self.edit_basic(ui, pal, layer, key, &rec),
            Some(Mode::Multimedia) => self.edit_multimedia(ui, pal, layer, key, &rec),
            Some(Mode::Mouse) => self.edit_mouse(ui, pal, layer, key, &rec),
            Some(Mode::Led) => self.edit_led(ui, pal, &rec),
            Some(Mode::Macro) => {
                section_label(ui, pal, "Macro");
                hint(
                    ui,
                    pal,
                    "Macro payloads aren't fully decoded yet, so they're kept byte-for-byte. \
                     You can edit the raw record below.",
                );
                ui.add_space(6.0);
                self.edit_raw(ui, pal, layer, key, &rec);
            }
            None if rec.mode == 0 => {
                hint(ui, pal, "This key doesn't do anything yet. Choose an action above.");
            }
            None => {
                section_label(ui, pal, "Unknown action");
                hint(ui, pal, &format!("Mode {} isn't recognised. Edit the raw record:", rec.mode));
                ui.add_space(6.0);
                self.edit_raw(ui, pal, layer, key, &rec);
            }
        }

        if rec.mode != 0 {
            ui.add_space(14.0);
            section_label(ui, pal, "Timing");
            ui.horizontal(|ui| {
                ui.label("Delay");
                let mut delay = rec.delay;
                if ui
                    .add(egui::DragValue::new(&mut delay).range(0..=255))
                    .on_hover_text("Raw delay byte stored with the key (0–255)")
                    .changed()
                {
                    let mut r = rec.clone();
                    r.delay = delay;
                    self.set_record(layer, key, r);
                }
            });
        }

        ui.add_space(18.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(is_dirty, egui::Button::new("Revert key"))
                .on_hover_text("Undo your changes to this key")
                .clicked()
            {
                self.revert_key(layer, key);
            }
            if ui
                .add_enabled(rec.mode != 0, egui::Button::new("Clear key"))
                .on_hover_text("Remove this key's action")
                .clicked()
            {
                self.learning_slot = None;
                self.set_record(layer, key, KeyRecord::empty());
            }
        });
    }

    fn edit_basic(&mut self, ui: &mut egui::Ui, pal: &Palette, layer: usize, key: usize, rec: &KeyRecord) {
        section_label(ui, pal, "Keystrokes");
        if self.learning_slot.is_some() {
            egui::Frame::none()
                .fill(pal.accent.gamma_multiply(0.12))
                .rounding(6.0)
                .inner_margin(egui::Margin::symmetric(10.0, 6.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        egui::RichText::new(
                            "Press a key combination to record it. Esc cancels. \
                             (The Win/⌘ modifier can't be captured on macOS.)",
                        )
                        .color(pal.accent),
                    );
                });
            ui.add_space(6.0);
        }

        let slots = rec.slots();
        let len = (rec.seq_len as usize).min(protocol::SLOTS);
        if len == 0 {
            hint(ui, pal, "No keystrokes yet.");
        }
        let mut remove: Option<usize> = None;
        for (i, slot) in slots.iter().enumerate().take(len) {
            let learning = self.learning_slot == Some((layer, key, i));
            let frame = card_frame(pal)
                .stroke(egui::Stroke::new(
                    if learning { 1.5 } else { 1.0 },
                    if learning { pal.accent } else { pal.border },
                ))
                .inner_margin(egui::Margin::symmetric(10.0, 8.0));
            frame.show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    if len > 1 {
                        ui.label(egui::RichText::new(format!("{}", i + 1)).color(pal.faint).monospace());
                    }
                    egui::ComboBox::from_id_salt(("pick", layer, key, i))
                        .width(118.0)
                        .selected_text(display_key(slot.b1))
                        .height(360.0)
                        .show_ui(ui, |ui| {
                            for (group, entries) in hid_codes::key_presets() {
                                ui.label(egui::RichText::new(*group).color(pal.weak).size(11.5));
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
                    let learn = egui::Button::new(if learning { "Press a key…" } else { "Record" })
                        .selected(learning);
                    if ui
                        .add(learn)
                        .on_hover_text("Capture a key combination from your keyboard")
                        .clicked()
                    {
                        self.learning_slot = if learning { None } else { Some((layer, key, i)) };
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add_enabled(len > 1, egui::Button::new("×").frame(false))
                            .on_hover_text("Remove this keystroke")
                            .clicked()
                        {
                            remove = Some(i);
                        }
                    });
                });
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let bits: &[(u8, &str)] = &[
                        (hid_codes::MOD_LCTRL, "Ctrl"),
                        (hid_codes::MOD_LSHIFT, "Shift"),
                        (hid_codes::MOD_LALT, "Alt"),
                        (hid_codes::MOD_LGUI, "Win / ⌘"),
                    ];
                    for (bit, name) in bits {
                        let on = slot.b0 & bit != 0;
                        if ui.selectable_label(on, *name).clicked() {
                            let mut r = rec.clone();
                            r.set_slot(i, Slot { b0: slot.b0 ^ bit, b1: slot.b1 });
                            self.set_record(layer, key, r);
                        }
                    }
                });
            });
            ui.add_space(2.0);
        }

        if let Some(i) = remove {
            let mut r = rec.clone();
            for j in i..protocol::SLOTS - 1 {
                r.set_slot(j, slots[j + 1]);
            }
            r.set_slot(protocol::SLOTS - 1, Slot::default());
            r.seq_len = r.seq_len.saturating_sub(1);
            self.learning_slot = None;
            self.set_record(layer, key, r);
        }

        ui.add_space(4.0);
        if ui
            .add_enabled(len < protocol::SLOTS, egui::Button::new("+ Add keystroke"))
            .on_hover_text("Type another key after this one (up to 18)")
            .clicked()
        {
            let mut r = rec.clone();
            r.set_slot(len, Slot::default());
            r.seq_len = (len + 1) as u8;
            self.set_record(layer, key, r);
            self.learning_slot = Some((layer, key, len));
        }

        ui.add_space(8.0);
        ui.collapsing("Advanced", |ui| {
            let slot = slots[0];
            let mut code = slot.b1;
            ui.horizontal(|ui| {
                ui.label("First key code");
                let resp = ui.add(
                    egui::DragValue::new(&mut code)
                        .range(0..=0xE7)
                        .speed(0.1)
                        .hexadecimal(2, false, true)
                        .prefix("0x"),
                );
                if resp.changed() {
                    let mut r = rec.clone();
                    r.set_slot(0, Slot { b0: slot.b0, b1: code });
                    self.set_record(layer, key, r);
                }
            });
        });
    }

    fn edit_multimedia(&mut self, ui: &mut egui::Ui, pal: &Palette, layer: usize, key: usize, rec: &KeyRecord) {
        section_label(ui, pal, "Media key");
        let slot = rec.slots()[0];
        let selected = match hid_codes::consumer_name(slot.b0) {
            "?" => format!("Code 0x{:02X}", slot.b0),
            n => n.to_string(),
        };
        egui::ComboBox::from_id_salt(("media", layer, key))
            .width(ui.available_width().min(240.0))
            .selected_text(selected)
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
        ui.add_space(8.0);
        ui.collapsing("Advanced", |ui| {
            egui::Grid::new("media_raw").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                let mut b0 = slot.b0;
                let mut b1 = slot.b1;
                ui.label("Consumer code");
                let r0 = ui.add(egui::DragValue::new(&mut b0).hexadecimal(2, false, true).prefix("0x"));
                ui.end_row();
                ui.label("Page / aux");
                let r1 = ui.add(egui::DragValue::new(&mut b1).hexadecimal(2, false, true).prefix("0x"));
                ui.end_row();
                if r0.changed() || r1.changed() {
                    let mut r = rec.clone();
                    r.seq_len = 2;
                    r.set_slot(0, Slot { b0, b1 });
                    self.set_record(layer, key, r);
                }
            });
        });
    }

    fn edit_mouse(&mut self, ui: &mut egui::Ui, pal: &Palette, layer: usize, key: usize, rec: &KeyRecord) {
        section_label(ui, pal, "Mouse button");
        let mut b = rec.raw[protocol::off::MOUSE_BTN];
        let mut s = rec.raw[protocol::off::SUBTYPE];
        let mut p = rec.raw[protocol::off::MOUSE_PARAM];
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for (val, name) in [(1u8, "Left"), (2, "Right"), (4, "Middle")] {
                if ui.selectable_label(b == val, name).clicked() {
                    b = val;
                }
            }
        });
        ui.add_space(8.0);
        ui.collapsing("Advanced", |ui| {
            egui::Grid::new("mouse_raw").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.label("Button / action");
                ui.add(egui::DragValue::new(&mut b).hexadecimal(2, false, true).prefix("0x"));
                ui.end_row();
                ui.label("Subtype");
                ui.add(egui::DragValue::new(&mut s).hexadecimal(2, false, true).prefix("0x"));
                ui.end_row();
                ui.label("Parameter");
                ui.add(egui::DragValue::new(&mut p).hexadecimal(2, false, true).prefix("0x"));
                ui.end_row();
            });
        });
        // Commit once if the buttons or any advanced field changed. (The
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

    /// LED layer-indicator editor. The LED write uses the reserved-slot path
    /// (record[1]=0xB0) via `Cmd::WriteLed` on the worker thread — independent
    /// of the key-config dirty state.
    fn edit_led(&mut self, ui: &mut egui::Ui, pal: &Palette, rec: &KeyRecord) {
        let current_bitmask = rec.raw[protocol::off::LED_BITMASK];
        let colour_idx = ((current_bitmask >> 4) & 0x0F) as usize; // high nibble = group
        let key_bits = current_bitmask & 0x0F; // low nibble = key bitmask
        let colour_name = LED_COLOURS.iter().find(|(i, _)| *i == colour_idx).map(|(_, n)| *n);

        section_label(ui, pal, "Indicator colour");
        hint(ui, pal, "Changes are sent to the keypad straight away.");
        ui.add_space(6.0);
        let can_write = self.conn == ConnState::Connected && !self.busy;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(4.0, 4.0);
            for &(idx, name) in LED_COLOURS {
                let resp = ui
                    .add_enabled_ui(can_write, |ui| {
                        egui::Frame::none().show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                dot(ui, led_rgb(idx), 5.0);
                                ui.selectable_label(colour_idx == idx, name)
                            })
                            .inner
                        })
                        .inner
                    })
                    .inner;
                if resp.clicked() {
                    self.write_led(idx);
                }
            }
        });

        ui.add_space(8.0);
        ui.collapsing("Advanced", |ui| {
            ui.label(
                egui::RichText::new(format!(
                    "Bitmask 0x{current_bitmask:02X}: colour group {colour_idx} ({}), key bits 0x{key_bits:X}",
                    colour_name.unwrap_or("?"),
                ))
                .color(pal.weak),
            );
            ui.add_space(4.0);
            let mut buf = hex_bytes(&rec.to_bytes());
            ui.add(
                egui::TextEdit::multiline(&mut buf)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(f32::INFINITY)
                    .interactive(false),
            );
        });
    }

    fn edit_raw(&mut self, ui: &mut egui::Ui, pal: &Palette, layer: usize, key: usize, rec: &KeyRecord) {
        ui.label(egui::RichText::new("Raw 50-byte record (hex)").color(pal.weak).size(12.0));
        let mut buf = hex_bytes(&rec.to_bytes());
        let resp = egui::TextEdit::multiline(&mut buf)
            .font(egui::TextStyle::Monospace)
            .desired_width(f32::INFINITY)
            .show(ui)
            .response;
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

// ---- Look and feel ----------------------------------------------------------

/// Colours used by the custom-drawn parts of the UI, for the current theme.
#[derive(Clone, Copy)]
struct Palette {
    accent: egui::Color32,
    selected_fill: egui::Color32,
    card: egui::Color32,
    card_hover: egui::Color32,
    border: egui::Color32,
    border_strong: egui::Color32,
    text: egui::Color32,
    weak: egui::Color32,
    faint: egui::Color32,
    warn: egui::Color32,
    warn_fill: egui::Color32,
    danger: egui::Color32,
    ok: egui::Color32,
}

impl Palette {
    fn of_ctx(ctx: &egui::Context) -> Self {
        Self::new(ctx.style().visuals.dark_mode)
    }

    fn new(dark: bool) -> Self {
        use egui::Color32 as C;
        if dark {
            Self {
                accent: C::from_rgb(77, 139, 255),
                selected_fill: C::from_rgb(31, 47, 78),
                card: C::from_rgb(37, 39, 45),
                card_hover: C::from_rgb(45, 48, 56),
                border: C::from_rgb(56, 60, 70),
                border_strong: C::from_rgb(92, 98, 112),
                text: C::from_rgb(232, 234, 238),
                weak: C::from_rgb(158, 163, 174),
                faint: C::from_rgb(110, 115, 126),
                warn: C::from_rgb(240, 178, 60),
                warn_fill: C::from_rgb(58, 46, 22),
                danger: C::from_rgb(226, 80, 80),
                ok: C::from_rgb(76, 196, 116),
            }
        } else {
            Self {
                accent: C::from_rgb(37, 99, 235),
                selected_fill: C::from_rgb(235, 242, 255),
                card: C::WHITE,
                card_hover: C::from_rgb(246, 248, 252),
                border: C::from_rgb(222, 225, 232),
                border_strong: C::from_rgb(180, 186, 198),
                text: C::from_rgb(28, 31, 38),
                weak: C::from_rgb(105, 111, 124),
                faint: C::from_rgb(160, 165, 176),
                warn: C::from_rgb(200, 128, 10),
                warn_fill: C::from_rgb(255, 247, 228),
                danger: C::from_rgb(210, 50, 50),
                ok: C::from_rgb(22, 160, 80),
            }
        }
    }
}

/// Theme tweaks applied once at startup to both the light and dark styles.
fn setup_style(ctx: &egui::Context) {
    use egui::{FontFamily, FontId, TextStyle};
    ctx.all_styles_mut(|s| {
        s.text_styles = [
            (TextStyle::Heading, FontId::new(21.0, FontFamily::Proportional)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Small, FontId::new(11.5, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.spacing.interact_size.y = 26.0;
        s.spacing.menu_margin = egui::Margin::same(8.0);
        let v = &mut s.visuals;
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.rounding = egui::Rounding::same(6.0);
        }
        v.window_rounding = egui::Rounding::same(10.0);
        v.menu_rounding = egui::Rounding::same(8.0);
    });
    for dark in [false, true] {
        let theme = if dark { egui::Theme::Dark } else { egui::Theme::Light };
        let pal = Palette::new(dark);
        ctx.style_mut_of(theme, |s| {
            s.visuals.selection.bg_fill = pal.accent.gamma_multiply(if dark { 0.45 } else { 0.16 });
            s.visuals.selection.stroke = egui::Stroke::new(1.0, if dark { pal.text } else { pal.accent });
            s.visuals.hyperlink_color = pal.accent;
        });
    }
}

/// What a keypad card shows.
struct Card {
    corner: String,
    title: String,
    tag: Option<(&'static str, egui::Color32)>,
    dirty: bool,
    selected: bool,
    flash: Option<f32>,
    max_rows: usize,
}

/// Draw a clickable keypad card: small label top-left, the action in the
/// middle, a coloured mode tag bottom-left and a dot when modified.
fn paint_card(ui: &mut egui::Ui, size: egui::Vec2, card: &Card) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let pal = Palette::new(ui.visuals().dark_mode);
    let painter = ui.painter();
    let hovered = resp.hovered();
    let fill = if card.selected {
        pal.selected_fill
    } else if hovered {
        pal.card_hover
    } else {
        pal.card
    };
    let stroke = if card.selected {
        egui::Stroke::new(2.0, pal.accent)
    } else if hovered {
        egui::Stroke::new(1.0, pal.border_strong)
    } else {
        egui::Stroke::new(1.0, pal.border)
    };
    painter.rect(rect, 8.0, fill, stroke);
    if let Some(f) = card.flash {
        painter.rect_filled(rect, 8.0, pal.ok.gamma_multiply(0.5 * f));
    }

    let inner = rect.shrink2(egui::vec2(10.0, 7.0));
    painter.text(
        inner.left_top(),
        egui::Align2::LEFT_TOP,
        &card.corner,
        egui::FontId::proportional(11.5),
        pal.weak,
    );
    if card.dirty {
        painter.circle_filled(egui::pos2(inner.right() - 3.0, inner.top() + 6.0), 4.0, pal.warn);
    }
    if let Some((tag, color)) = card.tag {
        painter.text(
            inner.left_bottom(),
            egui::Align2::LEFT_BOTTOM,
            tag,
            egui::FontId::proportional(10.5),
            color,
        );
    }

    let (text, color) = if card.title.is_empty() {
        ("Unassigned".to_string(), pal.faint)
    } else {
        (card.title.clone(), pal.text)
    };
    let mut job = egui::text::LayoutJob::simple(text, egui::FontId::proportional(15.0), color, inner.width());
    job.wrap.max_rows = card.max_rows;
    job.wrap.break_anywhere = true;
    let galley = painter.layout_job(job);
    // Centre the title in the space between the corner label and the tag.
    let area_top = inner.top() + 15.0;
    let area_bottom = if card.tag.is_some() { inner.bottom() - 14.0 } else { inner.bottom() };
    let top = (area_top + area_bottom - galley.size().y) / 2.0;
    painter.galley(egui::pos2(inner.left(), top), galley, color);
    resp
}

fn card_frame(pal: &Palette) -> egui::Frame {
    egui::Frame::none()
        .fill(pal.card)
        .stroke(egui::Stroke::new(1.0, pal.border))
        .rounding(8.0)
}

/// Small uppercase heading for a group of controls.
fn section_label(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(
        egui::RichText::new(text.to_uppercase())
            .size(11.0)
            .strong()
            .color(pal.weak)
            .extra_letter_spacing(0.6),
    );
    ui.add_space(2.0);
}

fn hint(ui: &mut egui::Ui, pal: &Palette, text: &str) {
    ui.label(egui::RichText::new(text).color(pal.weak));
}

fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::none()
        .fill(color.gamma_multiply(0.16))
        .rounding(10.0)
        .inner_margin(egui::Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).color(color).size(12.0).strong());
        });
}

/// A filled circle laid out inline (status lights, colour swatches).
fn dot(ui: &mut egui::Ui, color: egui::Color32, radius: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

fn led_name(idx: usize) -> &'static str {
    LED_COLOURS.iter().find(|(i, _)| *i == idx).map(|(_, n)| *n).unwrap_or("?")
}

fn led_rgb(idx: usize) -> egui::Color32 {
    use egui::Color32 as C;
    match idx {
        1 => C::from_rgb(232, 64, 64),
        2 => C::from_rgb(245, 140, 40),
        3 => C::from_rgb(240, 200, 40),
        4 => C::from_rgb(52, 190, 90),
        5 => C::from_rgb(40, 190, 215),
        6 => C::from_rgb(60, 110, 240),
        7 => C::from_rgb(160, 85, 230),
        _ => C::GRAY,
    }
}

/// Short coloured tag shown on a card for its action type.
fn mode_tag(m: u8) -> Option<(&'static str, egui::Color32)> {
    use egui::Color32 as C;
    match Mode::from_byte(m) {
        _ if m == 0 => None,
        Some(Mode::Basic) => Some(("KEYSTROKE", C::from_rgb(64, 120, 235))),
        Some(Mode::Multimedia) => Some(("MEDIA", C::from_rgb(165, 90, 235))),
        Some(Mode::Mouse) => Some(("MOUSE", C::from_rgb(20, 160, 150))),
        Some(Mode::Led) => Some(("LED", C::from_rgb(40, 170, 90))),
        Some(Mode::Macro) => Some(("MACRO", C::from_rgb(230, 125, 30))),
        None => Some(("UNKNOWN", C::GRAY)),
    }
}

/// Human-friendly one-line summary of what a key does ("" when unassigned).
fn action_title(rec: &KeyRecord) -> String {
    match rec.mode_enum() {
        _ if rec.mode == 0 => String::new(),
        Some(Mode::Basic) => {
            let slots = rec.slots();
            let steps = &slots[..(rec.seq_len as usize).min(protocol::SLOTS)];
            if steps.is_empty() {
                return String::new();
            }
            // A run of plain single characters reads best as the text it types.
            let plain_text = steps.len() > 1
                && steps.iter().all(|s| s.b0 == 0 && hid_codes::key_name(s.b1).chars().count() == 1);
            if plain_text {
                return steps.iter().map(|s| hid_codes::key_name(s.b1)).collect();
            }
            steps
                .iter()
                .map(|s| format!("{}{}", hid_codes::mod_name(s.b0), display_key(s.b1)))
                .collect::<Vec<_>>()
                .join(", ")
        }
        Some(Mode::Multimedia) => match hid_codes::consumer_name(rec.slots()[0].b0) {
            "?" => format!("Media 0x{:02X}", rec.slots()[0].b0),
            n => n.to_string(),
        },
        Some(Mode::Mouse) => match rec.raw[protocol::off::MOUSE_BTN] {
            1 => "Left click".into(),
            2 => "Right click".into(),
            4 => "Middle click".into(),
            b => format!("Mouse 0x{b:02X}"),
        },
        Some(Mode::Led) => "Layer LED".into(),
        Some(Mode::Macro) => "Macro".into(),
        None => format!("Mode {}", rec.mode),
    }
}

/// Key name as shown on keycaps: single letters upper-cased, unset shown as "—".
fn display_key(code: u8) -> String {
    if code == 0 {
        return "—".into();
    }
    let n = hid_codes::key_name(code);
    if n.len() == 1 {
        n.to_ascii_uppercase()
    } else {
        n.to_string()
    }
}

/// Display name for a mode byte (covers the known modes + a fallback).
fn mode_name(m: u8) -> &'static str {
    match Mode::from_byte(m) {
        _ if m == 0 => "None",
        Some(Mode::Basic) => "Keystroke",
        Some(Mode::Multimedia) => "Media key",
        Some(Mode::Mouse) => "Mouse",
        Some(Mode::Led) => "LED indicator",
        Some(Mode::Macro) => "Macro",
        None => "Unknown",
    }
}

fn hex_bytes(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
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
            .with_app_id("keytool")
            .with_inner_size([1120.0, 780.0])
            .with_min_inner_size([860.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Macro Keypad",
        options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            let mut app = KeyToolApp::default();
            if let Some(colours) = cc
                .storage
                .and_then(|s| eframe::get_value::<[Option<usize>; LAYERS]>(s, LED_COLOURS_KEY))
            {
                // Ignore anything out of range from an older/edited file.
                app.led_colours = colours.map(|c| c.filter(|i| (1..=7).contains(i)));
            }
            Ok(Box::new(app))
        }),
    )
}
