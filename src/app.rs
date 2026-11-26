use crate::gp::{self, Song};
use crate::score::{self, Layout, PaintCtx, ViewOpts};
use crate::synth::{Cmd, Player};
use crate::timing::{self, Timeline};
use eframe::egui::{self, Align2, Color32, FontId, Key, Pos2, Rect, Sense, Stroke, Vec2};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ViewMode {
    Both,
    Tab,
    Notation,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ThemeChoice {
    System,
    Light,
    Dark,
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Settings {
    library: Option<PathBuf>,
    recent: Vec<PathBuf>,
    zoom: f32,
    mode: ViewMode,
    show_library: bool,
    show_fretboard: bool,
    show_piano: bool,
    show_mixer: bool,
    metronome: bool,
    count_in: bool,
    follow: bool,
    speed: f32,
    theme: ThemeChoice,
    simple_sound: bool,
    paper: crate::pdf::Paper,
    /// Star ratings (1-5) by file path.
    ratings: std::collections::HashMap<String, u8>,
    rating_filter: RatingFilter,
    sort_by_rating: bool,
}

#[derive(Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum RatingFilter {
    All,
    Top,
    Good,
    Bad,
    Rated,
    Unrated,
}

impl RatingFilter {
    fn label(self) -> &'static str {
        match self {
            RatingFilter::All => "All files",
            RatingFilter::Top => "★★★★★ only",
            RatingFilter::Good => "Good: 4–5 stars",
            RatingFilter::Bad => "Weak: 1–2 stars",
            RatingFilter::Rated => "Any rating",
            RatingFilter::Unrated => "Not rated yet",
        }
    }
    fn keep(self, r: u8) -> bool {
        match self {
            RatingFilter::All => true,
            RatingFilter::Top => r == 5,
            RatingFilter::Good => r >= 4,
            RatingFilter::Bad => (1..=2).contains(&r),
            RatingFilter::Rated => r > 0,
            RatingFilter::Unrated => r == 0,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            library: None,
            recent: Vec::new(),
            zoom: 1.0,
            mode: ViewMode::Both,
            show_library: true,
            show_fretboard: true,
            show_piano: false,
            show_mixer: true,
            metronome: false,
            count_in: false,
            follow: true,
            speed: 1.0,
            theme: ThemeChoice::System,
            simple_sound: false,
            paper: crate::pdf::Paper::A4,
            ratings: Default::default(),
            rating_filter: RatingFilter::All,
            sort_by_rating: false,
        }
    }
}

struct LibEntry {
    path: PathBuf,
    rel: String,
    folder: String,
    name: String,
    lower: String,
}

struct TrackMix {
    mute: bool,
    solo: bool,
    volume: f32,
}

pub struct App {
    s: Settings,
    song: Arc<Song>,
    path: Option<PathBuf>,
    timeline: Timeline,
    player: Player,
    track: usize,
    all_tracks: bool,
    mix: Vec<TrackMix>,
    layout: Option<Layout>,
    cursor: (usize, u32),
    sel: Option<(usize, usize)>,
    looping: bool,
    lib: Vec<LibEntry>,
    lib_rx: Option<Receiver<Vec<LibEntry>>>,
    lib_scanning: bool,
    filter: String,
    error: Option<String>,
    show_info: bool,
    screenshot: Option<PathBuf>,
    frames: u32,
    shot_done: Arc<std::sync::atomic::AtomicBool>,
    last_follow_bar: Option<usize>,
    scroll_to: Option<f32>,
    cjk_loaded: bool,
    selected_lib: Option<PathBuf>,
    score_off: f32,
    print: Option<PrintDialog>,
    show_shortcuts: bool,
    #[cfg(target_os = "macos")]
    mac_menu: Option<crate::macmenu::MacMenu>,
}

struct PrintDialog {
    all_tracks: bool,
    mode: ViewMode,
    printers: Vec<String>,
    printer: Option<String>,
    copies: u32,
    status: Option<(bool, String)>,
}

fn is_tab_file(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("gp3") | Some("gp4")
    )
}

fn scan_library(root: PathBuf) -> Vec<LibEntry> {
    let mut out = Vec::new();
    let mut stack = vec![(root.clone(), 0)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                if depth < 8 && !name.ends_with(".app") && name != "node_modules" && name != "Library" {
                    stack.push((p, depth + 1));
                }
            } else if is_tab_file(&p) {
                let rel = p.strip_prefix(&root).map(|r| r.to_string_lossy().to_string()).unwrap_or(name.clone());
                let folder = p
                    .parent()
                    .and_then(|f| f.strip_prefix(&root).ok())
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_default();
                let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(name.clone());
                out.push(LibEntry { lower: rel.to_lowercase(), path: p, rel, folder, name: stem });
            }
        }
    }
    out.sort_by(|a, b| a.lower.cmp(&b.lower));
    out
}

impl App {
    pub fn new(cc: &eframe::CreationContext, open: Option<PathBuf>, screenshot: Option<PathBuf>) -> App {
        let mut s: Settings = if screenshot.is_some() {
            Settings::default()
        } else {
            cc.storage.and_then(|st| eframe::get_value(st, "settings")).unwrap_or_default()
        };
        if s.zoom < 0.5 || s.zoom > 3.0 {
            s.zoom = 1.0;
        }
        if s.library.is_none() {
            if let Some(home) = std::env::var_os("HOME") {
                let d = PathBuf::from(home).join("Downloads");
                if d.is_dir() {
                    s.library = Some(d);
                }
            }
        }
        let song = Arc::new(crate::demo::demo_song());
        let timeline = timing::build(&song);
        let player = Player::new();
        player.load(&timeline);
        let mut app = App {
            mix: Vec::new(),
            s,
            song,
            path: None,
            timeline,
            player,
            track: 0,
            all_tracks: false,
            layout: None,
            cursor: (0, gp::QUARTER),
            sel: None,
            looping: false,
            lib: Vec::new(),
            lib_rx: None,
            lib_scanning: false,
            filter: String::new(),
            error: None,
            show_info: false,
            screenshot,
            frames: 0,
            shot_done: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            last_follow_bar: None,
            scroll_to: None,
            cjk_loaded: false,
            selected_lib: None,
            score_off: 0.0,
            print: None,
            show_shortcuts: false,
            #[cfg(target_os = "macos")]
            mac_menu: None,
        };
        #[cfg(target_os = "macos")]
        {
            app.mac_menu = Some(crate::macmenu::MacMenu::new(&cc.egui_ctx));
        }
        // options used by automated checks
        if let Ok(v) = std::env::var("FRETBOOK_VIEW") {
            for part in v.split(',') {
                match part {
                    "tab" => app.s.mode = ViewMode::Tab,
                    "dark" => app.s.theme = ThemeChoice::Dark,
                    "print" => app.open_print(),
                    "light" => app.s.theme = ThemeChoice::Light,
                    "notation" => app.s.mode = ViewMode::Notation,
                    "all" => app.all_tracks = true,
                    _ => {}
                }
            }
        }
        app.reset_mix();
        app.apply_player_settings();
        app.style(&cc.egui_ctx);
        cc.egui_ctx.set_fonts(build_fonts(false));
        app.style(&cc.egui_ctx);
        if let Some(lib) = app.s.library.clone() {
            app.start_scan(lib, &cc.egui_ctx);
        }
        if let Some(p) = open {
            app.open(&p, &cc.egui_ctx);
        }
        app
    }

    fn style(&self, ctx: &egui::Context) {
        ctx.set_theme(match self.s.theme {
            ThemeChoice::System => egui::ThemePreference::System,
            ThemeChoice::Light => egui::ThemePreference::Light,
            ThemeChoice::Dark => egui::ThemePreference::Dark,
        });
        ctx.global_style_mut(|st| {
            st.spacing.item_spacing = Vec2::new(6.0, 5.0);
            st.spacing.button_padding = Vec2::new(7.0, 3.0);
        });
    }

    fn start_scan(&mut self, root: PathBuf, ctx: &egui::Context) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.lib_scanning = true;
        std::thread::spawn(move || {
            let list = scan_library(root);
            let _ = tx.send(list);
            ctx.request_repaint();
        });
        self.lib_rx = Some(rx);
    }

    fn reset_mix(&mut self) {
        self.mix = self
            .song
            .tracks
            .iter()
            .map(|_| TrackMix { mute: false, solo: false, volume: 1.0 })
            .collect();
        self.send_gains();
    }

    fn send_gains(&self) {
        let any_solo = self.mix.iter().any(|m| m.solo);
        let g: Vec<f32> = self
            .mix
            .iter()
            .map(|m| if m.mute || (any_solo && !m.solo) { 0.0 } else { m.volume })
            .collect();
        self.player.send(Cmd::TrackGain(g));
    }

    fn apply_player_settings(&self) {
        self.player.send(Cmd::Speed(self.s.speed));
        self.player.send(Cmd::Metronome(self.s.metronome));
        self.player.send(Cmd::CountIn(self.s.count_in));
        self.player.send(Cmd::UseSampler(!self.s.simple_sound));
    }

    fn open(&mut self, p: &Path, ctx: &egui::Context) {
        match gp::read_song_file(p) {
            Ok(song) => {
                self.player.send(Cmd::Stop);
                self.ensure_fonts(&song, ctx);
                self.song = Arc::new(song);
                self.timeline = timing::build(&self.song);
                self.player.load(&self.timeline);
                self.path = Some(p.to_path_buf());
                self.track = 0;
                self.all_tracks = std::env::var("FRETBOOK_VIEW").map(|v| v.contains("all")).unwrap_or(false);
                self.cursor = (0, self.song.headers.first().map(|h| h.start).unwrap_or(0));
                self.sel = None;
                self.looping = false;
                self.player.send(Cmd::Loop(None));
                self.layout = None;
                self.error = None;
                self.scroll_to = Some(0.0);
                self.reset_mix();
                self.s.recent.retain(|r| r != p);
                self.s.recent.insert(0, p.to_path_buf());
                self.s.recent.truncate(12);
                self.selected_lib = Some(p.to_path_buf());
                let title = if self.song.title.is_empty() {
                    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
                } else {
                    self.song.title.clone()
                };
                ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!("{title} — Fretbook")));
            }
            Err(e) => {
                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                self.error = Some(format!("Couldn't open “{name}”: {e}"));
            }
        }
    }

    fn ensure_fonts(&mut self, song: &Song, ctx: &egui::Context) {
        if self.cjk_loaded {
            return;
        }
        let mut texts = vec![song.title.clone(), song.artist.clone(), song.subtitle.clone(), song.album.clone()];
        texts.extend(song.tracks.iter().map(|t| t.name.clone()));
        let needs = texts.iter().any(|t| t.chars().any(|c| c as u32 >= 0x2E80));
        if needs {
            ctx.set_fonts(build_fonts(true));
            self.cjk_loaded = true;
        }
    }

    fn view_opts(&self, width: f32) -> ViewOpts {
        let tracks = if self.all_tracks {
            (0..self.song.tracks.len()).collect()
        } else {
            vec![self.track.min(self.song.tracks.len().saturating_sub(1))]
        };
        ViewOpts {
            zoom: self.s.zoom,
            notation: self.s.mode != ViewMode::Tab,
            tab: self.s.mode != ViewMode::Notation,
            tracks,
            width,
        }
    }

    fn play_pause(&mut self) {
        if self.player.is_playing() {
            self.player.send(Cmd::Stop);
            // keep the cursor where playback stopped
            if let Some((h, t)) = self.timeline.locate(&self.song, self.player.position()) {
                self.cursor = (h, self.beat_start(h, t));
            }
        } else {
            let start = if self.looping {
                if let Some((a, _)) = self.sel {
                    self.timeline.time_of(&self.song, a, self.song.headers[a].start)
                } else {
                    self.cursor_time()
                }
            } else {
                self.cursor_time()
            };
            self.player.send(Cmd::Play(start));
            self.last_follow_bar = None;
        }
    }

    fn stop(&mut self) {
        self.player.send(Cmd::Stop);
        self.cursor = (0, self.song.headers.first().map(|h| h.start).unwrap_or(0));
        self.player.send(Cmd::Seek(0.0));
        self.scroll_to = Some(0.0);
    }

    fn cursor_time(&self) -> f64 {
        self.timeline.time_of(&self.song, self.cursor.0, self.cursor.1)
    }

    fn beat_start(&self, header: usize, tick: u32) -> u32 {
        let t = &self.song.tracks[self.track.min(self.song.tracks.len() - 1)];
        t.measures
            .get(header)
            .and_then(|m| m.beats.iter().rev().find(|b| b.start <= tick))
            .map(|b| b.start)
            .unwrap_or(self.song.headers[header].start)
    }

    fn update_loop(&self) {
        if self.looping {
            if let Some((a, b)) = self.sel {
                let t0 = self.timeline.time_of(&self.song, a, self.song.headers[a].start);
                let t1 = self.timeline.bar_end_time(b);
                self.player.send(Cmd::Loop(Some((t0, t1))));
                return;
            }
        }
        self.player.send(Cmd::Loop(None));
    }

    fn move_bar(&mut self, delta: i32) {
        let n = self.song.headers.len() as i32;
        if n == 0 {
            return;
        }
        let h = (self.cursor.0 as i32 + delta).clamp(0, n - 1) as usize;
        self.cursor = (h, self.song.headers[h].start);
        if self.player.is_playing() {
            self.player.send(Cmd::Play(self.cursor_time()));
        }
        if let Some(l) = &self.layout {
            if let Some((y, _)) = score::bar_y(l, h) {
                self.scroll_to = Some((y - 40.0).max(0.0));
            }
        }
    }

    fn export_midi(&mut self) {
        let name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "song".into());
        if let Some(out) = rfd::FileDialog::new().set_file_name(format!("{name}.mid")).add_filter("MIDI file", &["mid"]).save_file() {
            let bytes = crate::midi::export(&self.song, &self.timeline);
            if let Err(e) = std::fs::write(&out, bytes) {
                self.error = Some(format!("Couldn't save the MIDI file: {e}"));
            }
        }
    }

    fn open_dialog(&mut self, ctx: &egui::Context) {
        let mut d = rfd::FileDialog::new().add_filter("Tab files", &["gp3", "gp4"]);
        if let Some(l) = &self.s.library {
            d = d.set_directory(l);
        }
        if let Some(p) = d.pick_file() {
            self.open(&p, ctx);
        }
    }

    fn choose_library(&mut self, ctx: &egui::Context) {
        if let Some(dir) = rfd::FileDialog::new().pick_folder() {
            self.s.library = Some(dir.clone());
            self.lib.clear();
            self.start_scan(dir, ctx);
        }
    }

    // ------------------------------------------------------------------

    fn keys(&mut self, ctx: &egui::Context) {
        if ctx.memory(|m| m.focused().is_some()) {
            return;
        }
        let (space, left, right, home, plus, minus, up, down, open) = ctx.input(|i| {
            (
                i.key_pressed(Key::Space),
                i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::ArrowRight),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                i.key_pressed(Key::Minus),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::ArrowDown),
                !cfg!(target_os = "macos") && i.modifiers.command && i.key_pressed(Key::O),
            )
        });
        if let Some(path) = self.path.clone() {
            let keys = [(Key::Num0, 0u8), (Key::Num1, 1), (Key::Num2, 2), (Key::Num3, 3), (Key::Num4, 4), (Key::Num5, 5)];
            for (k, r) in keys {
                if ctx.input(|i| !i.modifiers.command && i.key_pressed(k)) {
                    self.set_rating(&path, r);
                }
            }
        }
        if !cfg!(target_os = "macos") && ctx.input(|i| i.modifiers.command && i.key_pressed(Key::P)) {
            self.open_print();
        }
        if space {
            self.play_pause();
        }
        if left {
            self.move_bar(-1);
        }
        if right {
            self.move_bar(1);
        }
        if home {
            self.stop();
        }
        if plus {
            self.s.zoom = (self.s.zoom + 0.1).min(2.5);
        }
        if minus {
            self.s.zoom = (self.s.zoom - 0.1).max(0.6);
        }
        let n = self.song.tracks.len();
        if up && self.track > 0 {
            self.track -= 1;
        }
        if down && self.track + 1 < n {
            self.track += 1;
        }
        if open {
            self.open_dialog(ctx);
        }
    }

    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn menu(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("Open file…").clicked() {
                    ui.close();
                    self.open_dialog(&ctx);
                }
                if ui.button("Choose library folder…").clicked() {
                    ui.close();
                    self.choose_library(&ctx);
                }
                ui.menu_button("Open recent", |ui| {
                    let recent = self.s.recent.clone();
                    if recent.is_empty() {
                        ui.label("Nothing yet");
                    }
                    for r in recent {
                        let name = r.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                        if ui.button(name).clicked() {
                            ui.close();
                            self.open(&r, &ctx);
                        }
                    }
                });
                ui.separator();
                if ui.button("Print…    ⌘P").clicked() {
                    ui.close();
                    self.open_print();
                }
                if ui.button("Save as PDF…").clicked() {
                    ui.close();
                    self.open_print();
                    self.save_pdf(&ctx);
                }
                if ui.button("Export as MIDI…").clicked() {
                    ui.close();
                    self.export_midi();
                }
                if ui.button("Song information").clicked() {
                    ui.close();
                    self.show_info = true;
                }
                ui.separator();
                if ui.button("Quit").clicked() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
            ui.menu_button("View", |ui| {
                ui.radio_value(&mut self.s.mode, ViewMode::Both, "Notation and tab");
                ui.radio_value(&mut self.s.mode, ViewMode::Tab, "Tab only");
                ui.radio_value(&mut self.s.mode, ViewMode::Notation, "Notation only");
                ui.separator();
                ui.checkbox(&mut self.all_tracks, "Show all tracks together");
                ui.checkbox(&mut self.s.show_library, "Library");
                ui.checkbox(&mut self.s.show_mixer, "Tracks panel");
                ui.checkbox(&mut self.s.show_fretboard, "Fretboard");
                ui.checkbox(&mut self.s.show_piano, "Piano");
                ui.separator();
                if ui.button("Zoom in").clicked() {
                    self.s.zoom = (self.s.zoom + 0.1).min(2.5);
                }
                if ui.button("Zoom out").clicked() {
                    self.s.zoom = (self.s.zoom - 0.1).max(0.6);
                }
                if ui.button("Actual size").clicked() {
                    self.s.zoom = 1.0;
                }
                ui.separator();
                ui.label("Appearance");
                let mut changed = false;
                changed |= ui.radio_value(&mut self.s.theme, ThemeChoice::System, "Same as my Mac").changed();
                changed |= ui.radio_value(&mut self.s.theme, ThemeChoice::Light, "Light").changed();
                changed |= ui.radio_value(&mut self.s.theme, ThemeChoice::Dark, "Dark").changed();
                if changed {
                    self.style(&ctx);
                }
            });
            ui.menu_button("Playback", |ui| {
                if ui.button("Play / Pause    Space").clicked() {
                    ui.close();
                    self.play_pause();
                }
                if ui.button("Back to start").clicked() {
                    ui.close();
                    self.stop();
                }
                ui.separator();
                if ui.checkbox(&mut self.s.metronome, "Metronome").changed() {
                    self.apply_player_settings();
                }
                if ui.checkbox(&mut self.s.count_in, "Count in").changed() {
                    self.apply_player_settings();
                }
                ui.checkbox(&mut self.s.follow, "Follow the cursor");
                ui.separator();
                let mut realistic = !self.s.simple_sound;
                if ui.checkbox(&mut realistic, "Realistic instruments (recorded samples)").on_hover_text("Turn off for the simple built-in synthesizer").changed() {
                    self.s.simple_sound = !realistic;
                    self.apply_player_settings();
                }
                if ui.checkbox(&mut self.looping, "Loop the selected bars").changed() {
                    self.update_loop();
                }
            });
            ui.menu_button("Help", |ui| {
                if ui.button("Keyboard shortcuts").clicked() {
                    ui.close();
                    self.show_shortcuts = true;
                }
            });
        });
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let playing = self.player.is_playing();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            if icon_button(ui, Icon::Start, false).on_hover_text("Back to start (Home)").clicked() {
                self.stop();
            }
            if icon_button(ui, Icon::Prev, false).on_hover_text("Previous bar (←)").clicked() {
                self.move_bar(-1);
            }
            let icon = if playing { Icon::Pause } else { Icon::Play };
            if icon_button(ui, icon, true).on_hover_text("Play / Pause (Space)").clicked() {
                self.play_pause();
            }
            if icon_button(ui, Icon::Next, false).on_hover_text("Next bar (→)").clicked() {
                self.move_bar(1);
            }
            ui.separator();
            ui.checkbox(&mut self.s.show_piano, "Piano").on_hover_text("Show the piano keyboard");
            ui.separator();
            // position
            let (bar, time) = if playing {
                let pos = self.player.position();
                (self.timeline.locate(&self.song, pos).map(|x| x.0).unwrap_or(0), pos)
            } else {
                (self.cursor.0, self.cursor_time())
            };
            let total = self.timeline.total;
            let fmt = |t: f64| format!("{}:{:02}", (t / 60.0) as u32, (t % 60.0) as u32);
            ui.label(
                egui::RichText::new(format!("Bar {}/{}   {} / {}", bar + 1, self.song.headers.len(), fmt(time), fmt(total)))
                    .monospace()
                    .size(13.0),
            );
            ui.separator();
            ui.label("Speed");
            let resp = ui.add(
                egui::Slider::new(&mut self.s.speed, 0.25..=1.5)
                    .custom_formatter(|v, _| format!("{:.0}%", v * 100.0))
                    .step_by(0.05),
            );
            if resp.changed() {
                self.apply_player_settings();
            }
            ui.separator();
            if ui.toggle_value(&mut self.s.metronome, "Metronome").changed() {
                self.apply_player_settings();
            }
            if ui.toggle_value(&mut self.looping, "Loop").on_hover_text("Shift-click bars in the score to choose what loops").changed() {
                self.update_loop();
            }
            ui.separator();
            egui::ComboBox::from_id_salt("view_mode")
                .selected_text(match self.s.mode {
                    ViewMode::Both => "Notation + Tab",
                    ViewMode::Tab => "Tab",
                    ViewMode::Notation => "Notation",
                })
                .width(120.0)
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.s.mode, ViewMode::Both, "Notation + Tab");
                    ui.selectable_value(&mut self.s.mode, ViewMode::Tab, "Tab");
                    ui.selectable_value(&mut self.s.mode, ViewMode::Notation, "Notation");
                });
            if ui.button("−").on_hover_text("Zoom out").clicked() {
                self.s.zoom = (self.s.zoom - 0.1).max(0.6);
            }
            ui.label(format!("{:.0}%", self.s.zoom * 100.0));
            if ui.button("+").on_hover_text("Zoom in").clicked() {
                self.s.zoom = (self.s.zoom + 0.1).min(2.5);
            }
            ui.separator();
            let is_dark = ui.ctx().theme() == egui::Theme::Dark;
            let (icon, tip) = if is_dark { (Icon::Sun, "Switch to light mode") } else { (Icon::Moon, "Switch to dark mode") };
            if icon_button(ui, icon, false).on_hover_text(tip).clicked() {
                self.s.theme = if is_dark { ThemeChoice::Light } else { ThemeChoice::Dark };
                let ctx = ui.ctx().clone();
                self.style(&ctx);
            }
        });
    }

    fn rating_of(&self, p: &Path) -> u8 {
        self.s.ratings.get(p.to_string_lossy().as_ref()).copied().unwrap_or(0)
    }

    fn set_rating(&mut self, p: &Path, r: u8) {
        let key = p.to_string_lossy().to_string();
        if r == 0 {
            self.s.ratings.remove(&key);
        } else {
            self.s.ratings.insert(key, r.min(5));
        }
    }

    fn library_panel(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if let Some(rx) = &self.lib_rx {
            if let Ok(list) = rx.try_recv() {
                self.lib = list;
                // automated check: sample ratings
                if self.screenshot.is_some() && std::env::var("FRETBOOK_VIEW").map(|v| v.contains("rate")).unwrap_or(false) {
                    for (e, r) in self.lib.iter().zip([5u8, 3, 0, 4, 1, 2, 5, 0, 4]) {
                        if r > 0 {
                            self.s.ratings.insert(e.path.to_string_lossy().to_string(), r);
                        }
                    }
                    if let Some(p) = &self.path {
                        self.s.ratings.insert(p.to_string_lossy().to_string(), 4);
                    }
                }
                self.lib_scanning = false;
                self.lib_rx = None;
            }
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Library");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Folder…").on_hover_text("Choose the folder with your tab files").clicked() {
                    self.choose_library(&ctx);
                }
            });
        });
        if let Some(l) = &self.s.library {
            ui.label(egui::RichText::new(l.to_string_lossy()).small().weak());
        }
        ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Search songs, artists, folders").desired_width(f32::INFINITY));
        if self.lib_scanning {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Looking for tab files…");
            });
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_id_salt("rating_filter")
                .selected_text(self.s.rating_filter.label())
                .width(150.0)
                .show_ui(ui, |ui| {
                    for f in [RatingFilter::All, RatingFilter::Top, RatingFilter::Good, RatingFilter::Bad, RatingFilter::Rated, RatingFilter::Unrated] {
                        ui.selectable_value(&mut self.s.rating_filter, f, f.label());
                    }
                });
            ui.toggle_value(&mut self.s.sort_by_rating, "Best first").on_hover_text("Sort by your rating, highest first");
        });
        let words: Vec<String> = self.filter.to_lowercase().split_whitespace().map(|s| s.to_string()).collect();
        let mut shown: Vec<usize> = self
            .lib
            .iter()
            .enumerate()
            .filter(|(_, e)| words.iter().all(|w| e.lower.contains(w.as_str())))
            .filter(|(_, e)| self.s.rating_filter.keep(self.rating_of(&e.path)))
            .map(|(i, _)| i)
            .collect();
        if self.s.sort_by_rating {
            shown.sort_by_key(|&i| std::cmp::Reverse(self.rating_of(&self.lib[i].path)));
        }
        ui.label(egui::RichText::new(format!("{} of {} files", shown.len(), self.lib.len())).small().weak());
        ui.separator();
        let row_h = 34.0;
        let mut to_open = None;
        let mut new_rating: Option<(PathBuf, u8)> = None;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, shown.len(), |ui, range| {
            for &i in &shown[range] {
                let e = &self.lib[i];
                let rating = self.rating_of(&e.path);
                let selected = self.selected_lib.as_deref() == Some(e.path.as_path());
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h - 2.0), Sense::click());
                let vis = ui.visuals();
                if selected {
                    ui.painter().rect_filled(rect, 4.0, vis.selection.bg_fill);
                } else if resp.hovered() {
                    ui.painter().rect_filled(rect, 4.0, vis.widgets.hovered.weak_bg_fill);
                }
                let ext = e.path.extension().map(|x| x.to_string_lossy().to_uppercase()).unwrap_or_default();
                let badge = Rect::from_min_size(rect.min + Vec2::new(4.0, 7.0), Vec2::new(30.0, 18.0));
                let bcol = match ext.as_str() {
                    "GP3" => Color32::from_rgb(70, 140, 90),
                    "GP4" => Color32::from_rgb(60, 110, 200),
                    _ => Color32::GRAY,
                };
                ui.painter().rect_filled(badge, 3.0, bcol);
                ui.painter().text(badge.center(), Align2::CENTER_CENTER, &ext, FontId::proportional(10.0), Color32::WHITE);
                let tc = if selected { vis.selection.stroke.color } else { vis.text_color() };
                // stars on the right: shown when rated or when the pointer is over the row
                let star_size = 11.0;
                let stars_rect = Rect::from_min_size(
                    Pos2::new(rect.max.x - 5.0 * star_size - 6.0, rect.min.y + 3.0),
                    Vec2::new(5.0 * star_size, star_size + 2.0),
                );
                let pointer = ui.input(|inp| inp.pointer.hover_pos());
                let show_stars = rating > 0 || resp.hovered();
                let hover_value = pointer.filter(|p| stars_rect.expand(2.0).contains(*p)).map(|p| (((p.x - stars_rect.min.x) / star_size).floor() as i32 + 1).clamp(1, 5) as u8);
                if show_stars {
                    draw_stars(ui.painter(), stars_rect.min, star_size, hover_value.unwrap_or(rating), hover_value.is_some());
                }
                let name_right = if show_stars { stars_rect.min.x - 4.0 } else { rect.max.x };
                let clip = Rect::from_min_max(rect.min + Vec2::new(40.0, 0.0), Pos2::new(name_right, rect.max.y));
                let painter = ui.painter_at(clip);
                painter.text(rect.min + Vec2::new(40.0, 3.0), Align2::LEFT_TOP, &e.name, FontId::proportional(13.0), tc);
                painter.text(rect.min + Vec2::new(40.0, 19.0), Align2::LEFT_TOP, &e.folder, FontId::proportional(10.5), vis.weak_text_color());
                if resp.clicked() {
                    if let Some(v) = hover_value {
                        // clicking the current rating again clears it
                        new_rating = Some((e.path.clone(), if v == rating { 0 } else { v }));
                    } else {
                        to_open = Some(e.path.clone());
                    }
                }
                if hover_value.is_some() {
                    resp.on_hover_text("Click to rate (click again to clear)");
                } else {
                    resp.on_hover_text(&e.rel);
                }
            }
        });
        if let Some((p, r)) = new_rating {
            self.set_rating(&p, r);
        }
        if let Some(p) = to_open {
            self.open(&p, &ctx);
        }
        if !self.lib_scanning && !self.lib.is_empty() && shown.is_empty() {
            ui.label(egui::RichText::new("No files match. Try another search or rating filter.").weak());
        }
        if !self.lib_scanning && self.lib.is_empty() {
            ui.label("No .gp3 / .gp4 files found here. Choose another folder with “Folder…”.");
        }
    }

    fn mixer_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.heading("Tracks");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.toggle_value(&mut self.all_tracks, "All").on_hover_text("Show all tracks together");
            });
        });
        ui.separator();
        let mut changed = false;
        let song = self.song.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, t) in song.tracks.iter().enumerate() {
                let selected = i == self.track;
                let frame = egui::Frame::new()
                    .inner_margin(6.0)
                    .corner_radius(6.0)
                    .fill(if selected { ui.visuals().selection.bg_fill.gamma_multiply(0.5) } else { ui.visuals().faint_bg_color });
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    let r = ui.horizontal(|ui| {
                        let (sw, _) = ui.allocate_exact_size(Vec2::new(10.0, 28.0), Sense::hover());
                        ui.painter().rect_filled(sw, 3.0, Color32::from_rgb(t.color[0], t.color[1], t.color[2]));
                        ui.vertical(|ui| {
                            let name = if t.name.is_empty() { format!("Track {}", i + 1) } else { t.name.clone() };
                            ui.label(egui::RichText::new(name).strong());
                            let inst = if t.percussion { "Drums".to_string() } else { gp::instrument_name(t.channel.instrument).to_string() };
                            let detail = if t.percussion {
                                inst
                            } else {
                                format!("{} · {} strings{}", inst, t.strings.len(), if t.capo > 0 { format!(" · capo {}", t.capo) } else { String::new() })
                            };
                            ui.label(egui::RichText::new(detail).small().weak());
                        });
                    });
                    if r.response.interact(Sense::click()).clicked() {
                        self.track = i;
                        self.all_tracks = false;
                    }
                    ui.horizontal(|ui| {
                        let m = &mut self.mix[i];
                        let mute = egui::Button::new(egui::RichText::new("M").strong())
                            .selected(m.mute)
                            .min_size(Vec2::new(24.0, 20.0));
                        if ui.add(mute).on_hover_text("Mute").clicked() {
                            m.mute = !m.mute;
                            changed = true;
                        }
                        let solo = egui::Button::new(egui::RichText::new("S").strong())
                            .selected(m.solo)
                            .min_size(Vec2::new(24.0, 20.0));
                        if ui.add(solo).on_hover_text("Solo").clicked() {
                            m.solo = !m.solo;
                            changed = true;
                        }
                        if ui.add(egui::Slider::new(&mut m.volume, 0.0..=1.5).show_value(false)).on_hover_text("Volume").changed() {
                            changed = true;
                        }
                        if ui.small_button("View").clicked() {
                            self.track = i;
                            self.all_tracks = false;
                        }
                    });
                });
                ui.add_space(3.0);
            }
        });
        if changed {
            self.send_gains();
        }
    }

    fn fretboard(&mut self, ui: &mut egui::Ui) {
        let song = self.song.clone();
        let Some(track) = song.tracks.get(self.track) else { return };
        let rect = ui.available_rect_before_wrap();
        let (rect, resp) = ui.allocate_exact_size(rect.size(), Sense::click());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, Color32::from_rgb(38, 32, 28));
        if track.percussion {
            p.text(rect.center(), Align2::CENTER_CENTER, "Drum track — no fretboard", FontId::proportional(14.0), Color32::LIGHT_GRAY);
            return;
        }
        let frets = track.frets.clamp(12, 24) as usize;
        let n = track.strings.len().max(1);
        // Leave room on top for fret numbers and on the left for string names + open-string markers.
        let neck = Rect::from_min_max(rect.min + Vec2::new(64.0, 22.0), rect.max - Vec2::new(14.0, 10.0));
        let nut_x = neck.min.x;
        // Fret positions with real (equal-temperament) proportions, slightly compressed so high frets stay usable.
        let scale = |f: f32| -> f32 { 1.0 - 2f32.powf(-f / 17.0) };
        let total = scale(frets as f32 + 0.6);
        let fx = |f: f32| nut_x + (neck.max.x - nut_x) * scale(f) / total;
        let fret_mid = |f: usize| (fx(f as f32 - 1.0) + fx(f as f32)) / 2.0;

        // Rosewood fingerboard with a subtle vertical shading (lighter in the middle).
        let wood_dark = Color32::from_rgb(52, 30, 18);
        let wood_light = Color32::from_rgb(84, 52, 32);
        let bands = 12;
        for i in 0..bands {
            let t0 = i as f32 / bands as f32;
            let t1 = (i + 1) as f32 / bands as f32;
            let k = 1.0 - ((t0 + t1) - 1.0).abs();
            let c = Color32::from_rgb(
                (wood_dark.r() as f32 + (wood_light.r() as f32 - wood_dark.r() as f32) * k) as u8,
                (wood_dark.g() as f32 + (wood_light.g() as f32 - wood_dark.g() as f32) * k) as u8,
                (wood_dark.b() as f32 + (wood_light.b() as f32 - wood_dark.b() as f32) * k) as u8,
            );
            p.rect_filled(
                Rect::from_min_max(Pos2::new(nut_x, neck.min.y + neck.height() * t0), Pos2::new(neck.max.x, neck.min.y + neck.height() * t1 + 0.5)),
                0.0,
                c,
            );
        }
        // Binding along the edges.
        p.line_segment([Pos2::new(nut_x, neck.min.y), Pos2::new(neck.max.x, neck.min.y)], Stroke::new(1.5, Color32::from_rgb(225, 215, 190)));
        p.line_segment([Pos2::new(nut_x, neck.max.y), Pos2::new(neck.max.x, neck.max.y)], Stroke::new(1.5, Color32::from_rgb(225, 215, 190)));

        // Inlays (pearl dots), drawn before frets and strings.
        let cy = neck.center().y;
        let inlay = Color32::from_rgb(232, 226, 210);
        for f in [3, 5, 7, 9, 15, 17, 19, 21] {
            if f <= frets {
                p.circle_filled(Pos2::new(fret_mid(f), cy), 6.0, inlay);
            }
        }
        for f in [12, 24] {
            if f <= frets {
                let h = neck.height() / 4.0;
                p.circle_filled(Pos2::new(fret_mid(f), cy - h), 6.0, inlay);
                p.circle_filled(Pos2::new(fret_mid(f), cy + h), 6.0, inlay);
            }
        }

        // Metal fret wires with a highlight, and fret numbers above the neck.
        for f in 1..=frets {
            let x = fx(f as f32);
            p.line_segment([Pos2::new(x, neck.min.y), Pos2::new(x, neck.max.y)], Stroke::new(3.0, Color32::from_rgb(150, 150, 150)));
            p.line_segment([Pos2::new(x - 0.5, neck.min.y), Pos2::new(x - 0.5, neck.max.y)], Stroke::new(1.0, Color32::from_rgb(235, 235, 235)));
            let marked = matches!(f, 3 | 5 | 7 | 9 | 12 | 15 | 17 | 19 | 21 | 24);
            let col = if marked { Color32::from_rgb(220, 220, 220) } else { Color32::from_rgb(140, 140, 140) };
            p.text(Pos2::new(fret_mid(f), neck.min.y - 4.0), Align2::CENTER_BOTTOM, f.to_string(), FontId::proportional(11.0), col);
        }
        // Bone nut.
        p.rect_filled(Rect::from_min_max(Pos2::new(nut_x - 6.0, neck.min.y - 1.0), Pos2::new(nut_x, neck.max.y + 1.0)), 1.0, Color32::from_rgb(240, 234, 214));

        // Strings: thicker and bronze-coloured for the wound (lower) strings.
        let sy = |s: usize| neck.min.y + 8.0 + (neck.height() - 16.0) * s as f32 / (n.max(2) - 1) as f32;
        for s in 0..n {
            let w = 1.0 + s as f32 * 0.45;
            let wound = n >= 6 && s >= n / 2;
            let col = if wound { Color32::from_rgb(205, 180, 135) } else { Color32::from_rgb(220, 220, 220) };
            let y = sy(s);
            p.line_segment([Pos2::new(nut_x - 6.0, y + 1.0), Pos2::new(neck.max.x, y + 1.0)], Stroke::new(w, Color32::from_black_alpha(110)));
            p.line_segment([Pos2::new(nut_x - 6.0, y), Pos2::new(neck.max.x, y)], Stroke::new(w, col));
            p.text(Pos2::new(rect.min.x + 10.0, y), Align2::LEFT_CENTER, gp::pitch_name(track.strings[s]), FontId::proportional(11.0), Color32::LIGHT_GRAY);
        }
        // notes at the cursor
        let (h, tick) = if self.player.is_playing() {
            self.timeline.locate(&song, self.player.position()).unwrap_or(self.cursor)
        } else {
            self.cursor
        };
        if let Some(beat) = track.measures.get(h).and_then(|m| m.beats.iter().rev().find(|b| b.start <= tick)) {
            for note in &beat.notes {
                let s = note.string as usize - 1;
                if s >= n {
                    continue;
                }
                let fret = note.fret.max(0) as usize;
                if fret > frets {
                    continue;
                }
                let x = if fret == 0 { nut_x - 20.0 } else { fret_mid(fret) };
                let c = Pos2::new(x, sy(s));
                let col = Color32::from_rgb(track.color[0].max(60), track.color[1].max(40), track.color[2].max(40));
                let r = (neck.height() / (n as f32 * 2.0)).clamp(8.0, 12.0);
                // Soft glow so the playing note stands out on the dark wood.
                for (i, a) in [(3.0, 40u8), (6.0, 22u8)] {
                    p.circle_filled(c, r + i, Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), a));
                }
                p.circle_filled(c, r, col);
                p.circle_stroke(c, r, Stroke::new(1.5, Color32::WHITE));
                let label = if note.kind == gp::NoteKind::Dead { "x".to_string() } else { gp::pitch_name(track.pitch_of(note)) };
                p.text(c, Align2::CENTER_CENTER, label, FontId::proportional(r.min(11.0)), Color32::WHITE);
            }
        }
        // click to hear a note
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let s = (0..n).min_by(|a, b| (sy(*a) - pos.y).abs().partial_cmp(&(sy(*b) - pos.y).abs()).unwrap()).unwrap_or(0);
                let mut fret = 0;
                if pos.x > nut_x {
                    for f in 1..=frets {
                        if pos.x < fx(f as f32) {
                            fret = f as i32;
                            break;
                        }
                    }
                }
                let pitch = track.strings[s] + track.capo + fret;
                self.player.send(Cmd::Preview(timing::NoteEvent {
                    time: 0.0,
                    dur: 1.2,
                    track: self.track,
                    string: 99,
                    pitch: pitch as f32,
                    velocity: 0.8,
                    sound: timing::sound_for(track.channel.instrument, false),
                    palm_mute: false,
                    dead: false,
                    vibrato: false,
                    hammer: false,
                    bend: None,
                    slide_to: 0.0,
                    slide: false,
                    pan: 0.0,
                    volume: 1.0,
                    program: track.channel.instrument,
                    harmonic: false,
                }));
            }
        }
    }

    /// An 88-key piano (A0–C8) that lights up the notes sounding on the current track.
    fn piano(&mut self, ui: &mut egui::Ui) {
        const LOW: i32 = 21; // A0
        const HIGH: i32 = 108; // C8
        let is_black = |p: i32| matches!(p.rem_euclid(12), 1 | 3 | 6 | 8 | 10);
        let song = self.song.clone();
        let Some(track) = song.tracks.get(self.track) else { return };
        let rect = ui.available_rect_before_wrap();
        let (rect, resp) = ui.allocate_exact_size(rect.size(), Sense::click());
        let p = ui.painter_at(rect);
        p.rect_filled(rect, 0.0, Color32::from_rgb(28, 28, 30));
        if track.percussion {
            p.text(rect.center(), Align2::CENTER_CENTER, "Drum track — no piano", FontId::proportional(14.0), Color32::LIGHT_GRAY);
            return;
        }

        // Pitches sounding at the cursor (or the playback position) on the current track.
        let (h, tick) = if self.player.is_playing() {
            self.timeline.locate(&song, self.player.position()).unwrap_or(self.cursor)
        } else {
            self.cursor
        };
        let active: Vec<i32> = track
            .measures
            .get(h)
            .and_then(|m| m.beats.iter().rev().find(|b| b.start <= tick))
            .map(|b| b.notes.iter().filter(|n| n.kind != gp::NoteKind::Dead).map(|n| track.pitch_of(n)).collect())
            .unwrap_or_default();
        let col = Color32::from_rgb(track.color[0].max(60), track.color[1].max(40), track.color[2].max(40));

        let kb = rect.shrink2(Vec2::new(8.0, 6.0));
        let whites: Vec<i32> = (LOW..=HIGH).filter(|p| !is_black(*p)).collect();
        let ww = kb.width() / whites.len() as f32;
        let bw = ww * 0.62;
        let bh = kb.height() * 0.62;
        let white_x = |p: i32| kb.min.x + whites.iter().position(|w| *w == p).unwrap_or(0) as f32 * ww;
        let key_rect = |p: i32| -> Rect {
            if is_black(p) {
                let x = white_x(p - 1) + ww - bw / 2.0;
                Rect::from_min_size(Pos2::new(x, kb.min.y), Vec2::new(bw, bh))
            } else {
                Rect::from_min_size(Pos2::new(white_x(p), kb.min.y), Vec2::new(ww, kb.height()))
            }
        };

        for &w in &whites {
            let r = key_rect(w).shrink2(Vec2::new(0.5, 0.0));
            let on = active.contains(&w);
            p.rect_filled(r, 2.0, if on { col } else { Color32::from_rgb(246, 244, 238) });
            p.rect_stroke(r, 2.0, Stroke::new(1.0, Color32::from_rgb(90, 90, 90)), egui::StrokeKind::Inside);
            // Label every C (and every active key) so the octave is easy to find.
            if on || w % 12 == 0 {
                let label = if w % 12 == 0 { format!("C{}", w / 12 - 1) } else { gp::pitch_name(w) };
                let tc = if on { Color32::WHITE } else { Color32::from_rgb(120, 120, 120) };
                p.text(Pos2::new(r.center().x, r.max.y - 4.0), Align2::CENTER_BOTTOM, label, FontId::proportional((ww * 0.45).clamp(7.0, 11.0)), tc);
            }
        }
        for b in (LOW..=HIGH).filter(|p| is_black(*p)) {
            let r = key_rect(b);
            let on = active.contains(&b);
            p.rect_filled(r, 2.0, if on { col } else { Color32::from_rgb(24, 24, 26) });
            if !on {
                // Simple highlight to give the black keys some depth.
                p.rect_filled(Rect::from_min_size(r.min + Vec2::new(2.0, 0.0), Vec2::new(r.width() - 4.0, r.height() - 5.0)), 1.5, Color32::from_rgb(52, 52, 56));
            }
        }
        // Thin felt strip along the top edge of the keys.
        p.rect_filled(Rect::from_min_size(kb.min, Vec2::new(kb.width(), 3.0)), 0.0, Color32::from_rgb(140, 20, 30));

        // Click a key to hear it.
        if resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let hit = (LOW..=HIGH)
                    .filter(|p| is_black(*p))
                    .find(|b| key_rect(*b).contains(pos))
                    .or_else(|| whites.iter().copied().find(|w| key_rect(*w).contains(pos)));
                if let Some(pitch) = hit {
                    self.player.send(Cmd::Preview(timing::NoteEvent {
                        time: 0.0,
                        dur: 1.2,
                        track: self.track,
                        string: 99,
                        pitch: pitch as f32,
                        velocity: 0.8,
                        sound: timing::sound_for(track.channel.instrument, false),
                        palm_mute: false,
                        dead: false,
                        vibrato: false,
                        hammer: false,
                        bend: None,
                        slide_to: 0.0,
                        slide: false,
                        pan: 0.0,
                        volume: 1.0,
                        program: track.channel.instrument,
                        harmonic: false,
                    }));
                }
            }
        }
    }

    fn score_view(&mut self, ui: &mut egui::Ui) {
        let width = ui.available_width();
        let page_w = (width - 24.0).clamp(400.0, 1400.0 * self.s.zoom.max(1.0));
        let opts = self.view_opts(page_w);
        if self.layout.as_ref().map(|l| l.opts != opts).unwrap_or(true) {
            self.layout = Some(score::layout(&self.song, &opts));
        }
        let layout = self.layout.clone().unwrap();
        let playing = self.player.is_playing();
        let pos = self.player.position();
        let play_at = if playing { self.timeline.locate(&self.song, pos) } else { None };

        // cursor beat (selected track)
        let (ch, ct) = play_at.unwrap_or(self.cursor);
        let track = &self.song.tracks[self.track.min(self.song.tracks.len() - 1)];
        let cursor = track.measures.get(ch).and_then(|m| {
            let b = m.beats.iter().rev().find(|b| b.start <= ct)?;
            Some((ch, b.start, b.start + b.duration.ticks()))
        });

        let mut scroll = egui::ScrollArea::vertical().id_salt("score").auto_shrink([false, false]);
        if let Some(y) = self.scroll_to.take() {
            scroll = scroll.vertical_scroll_offset(y);
        }
        // follow playback
        if playing && self.s.follow {
            if let Some((h, _)) = play_at {
                if self.last_follow_bar != Some(h) {
                    self.last_follow_bar = Some(h);
                    if let Some((y, sh)) = score::bar_y(&layout, h) {
                        let view_h = ui.available_height();
                        let off = self.score_off;
                        if y < off || y + sh > off + view_h {
                            scroll = scroll.vertical_scroll_offset((y - 30.0).max(0.0));
                        }
                    }
                }
            }
        }
        let song = self.song.clone();
        let out = scroll.show_viewport(ui, |ui, viewport| {
            let total = Vec2::new(width, layout.height + 20.0);
            let (rect, resp) = ui.allocate_exact_size(total, Sense::click());
            let page = Rect::from_min_size(
                Pos2::new(rect.min.x + (width - page_w) / 2.0, rect.min.y + 10.0),
                Vec2::new(page_w, layout.height),
            );
            let p = ui.painter();
            p.rect_filled(page.translate(Vec2::new(2.0, 3.0)), 2.0, Color32::from_black_alpha(40));
            p.rect_filled(page, 2.0, score::paper());
            let clip = Rect::from_min_size(rect.min + viewport.min.to_vec2(), viewport.size()).expand(50.0);
            let pc = PaintCtx {
                song: &song,
                origin: page.min,
                clip,
                cursor,
                selection: self.sel,
                playing_bar: play_at.map(|x| x.0),
                header: true,
            };
            score::paint(p, &layout, &pc);
            if resp.clicked() {
                if let Some(pp) = resp.interact_pointer_pos() {
                    let rel = Pos2::new(pp.x - page.min.x, pp.y - page.min.y);
                    if let Some((h, t, trk)) = score::hit(&layout, &song, rel) {
                        let shift = ui.input(|i| i.modifiers.shift);
                        if shift {
                            let a = self.sel.map(|s| s.0).unwrap_or(self.cursor.0);
                            self.sel = Some((a.min(h), a.max(h)));
                            self.looping = true;
                            self.update_loop();
                        } else {
                            self.sel = None;
                            if self.looping {
                                self.looping = false;
                                self.update_loop();
                            }
                            if layout.opts.tracks.len() > 1 {
                                self.track = trk;
                            }
                            self.cursor = (h, t);
                            if self.player.is_playing() {
                                self.player.send(Cmd::Play(self.cursor_time()));
                            }
                        }
                    }
                }
            }
        });
        self.score_off = out.state.clamped_offset().y;
    }

    fn open_print(&mut self) {
        if self.print.is_none() {
            let (printers, default) = crate::pdf::printers();
            let printer = default.or_else(|| printers.first().cloned());
            self.print = Some(PrintDialog {
                all_tracks: self.all_tracks,
                mode: self.s.mode,
                printers,
                printer,
                copies: 1,
                status: None,
            });
        }
    }

    fn print_job(&self) -> crate::pdf::PrintJob<'_> {
        let d = self.print.as_ref().unwrap();
        let tracks = if d.all_tracks {
            (0..self.song.tracks.len()).collect()
        } else {
            vec![self.track.min(self.song.tracks.len().saturating_sub(1))]
        };
        let file_name = self
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "Sample piece".into());
        crate::pdf::PrintJob {
            song: &self.song,
            tracks,
            notation: d.mode != ViewMode::Tab,
            tab: d.mode != ViewMode::Notation,
            paper: self.s.paper,
            file_name,
        }
    }

    fn pdf_name(&self) -> String {
        let job = self.print_job();
        let base = if self.song.title.is_empty() { job.file_name.clone() } else { self.song.title.clone() };
        let clean: String = base.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
        format!("{}.pdf", clean.trim())
    }

    fn temp_pdf(&mut self, ctx: &egui::Context) -> Option<PathBuf> {
        let bytes = crate::pdf::make_pdf(ctx, &self.print_job());
        let dir = std::env::temp_dir().join("Fretbook print");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(self.pdf_name());
        match std::fs::write(&path, bytes) {
            Ok(_) => Some(path),
            Err(e) => {
                if let Some(d) = &mut self.print {
                    d.status = Some((false, format!("Couldn't prepare the pages: {e}")));
                }
                None
            }
        }
    }

    fn save_pdf(&mut self, ctx: &egui::Context) {
        let name = self.pdf_name();
        if let Some(out) = rfd::FileDialog::new().set_file_name(name).add_filter("PDF", &["pdf"]).save_file() {
            let bytes = crate::pdf::make_pdf(ctx, &self.print_job());
            let msg = match std::fs::write(&out, bytes) {
                Ok(_) => (true, format!("Saved {}", out.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default())),
                Err(e) => (false, format!("Couldn't save: {e}")),
            };
            if let Some(d) = &mut self.print {
                d.status = Some(msg);
            }
        }
    }

    fn print_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let mut action = 0; // 1 print, 2 preview, 3 save
        let track_name = self
            .song
            .tracks
            .get(self.track)
            .map(|t| if t.name.is_empty() { format!("Track {}", self.track + 1) } else { t.name.clone() })
            .unwrap_or_default();
        let pages = crate::pdf::page_breaks(&self.print_job()).1.len();
        let mut paper = self.s.paper;
        let d = self.print.as_mut().unwrap();
        egui::Window::new("Print")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(380.0)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Grid::new("print_grid").num_columns(2).spacing([14.0, 10.0]).show(ui, |ui| {
                    ui.label("Tracks");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut d.all_tracks, false, format!("This track: {track_name}"));
                        ui.radio_value(&mut d.all_tracks, true, "All tracks together");
                    });
                    ui.end_row();
                    ui.label("Show");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut d.mode, ViewMode::Both, "Notation + Tab");
                        ui.radio_value(&mut d.mode, ViewMode::Tab, "Tab");
                        ui.radio_value(&mut d.mode, ViewMode::Notation, "Notation");
                    });
                    ui.end_row();
                    ui.label("Paper");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut paper, crate::pdf::Paper::A4, crate::pdf::Paper::A4.name());
                        ui.radio_value(&mut paper, crate::pdf::Paper::Letter, crate::pdf::Paper::Letter.name());
                    });
                    ui.end_row();
                    ui.label("Printer");
                    if d.printers.is_empty() {
                        ui.label(egui::RichText::new("No printer is set up on this Mac").weak());
                    } else {
                        egui::ComboBox::from_id_salt("printer")
                            .width(230.0)
                            .selected_text(d.printer.clone().unwrap_or_default().replace('_', " "))
                            .show_ui(ui, |ui| {
                                for p in d.printers.clone() {
                                    let label = p.replace('_', " ");
                                    ui.selectable_value(&mut d.printer, Some(p), label);
                                }
                            });
                    }
                    ui.end_row();
                    ui.label("Copies");
                    ui.add(egui::DragValue::new(&mut d.copies).range(1..=99));
                    ui.end_row();
                });
                ui.add_space(4.0);
                ui.label(egui::RichText::new(format!("{pages} page{}", if pages == 1 { "" } else { "s" })).weak());
                if let Some((ok, msg)) = &d.status {
                    let col = if *ok { Color32::from_rgb(40, 150, 70) } else { Color32::from_rgb(200, 60, 50) };
                    ui.colored_label(col, msg);
                }
                ui.separator();
                ui.horizontal(|ui| {
                    let can_print = d.printer.is_some();
                    let print_btn = egui::Button::new(egui::RichText::new("Print").strong().color(Color32::WHITE))
                        .fill(Color32::from_rgb(52, 120, 220))
                        .min_size(Vec2::new(80.0, 26.0));
                    if ui.add_enabled(can_print, print_btn).clicked() {
                        action = 1;
                    }
                    if ui
                        .button("Open in Preview")
                        .on_hover_text("Opens the pages in Preview, where ⌘P shows the full Mac print dialog")
                        .clicked()
                    {
                        action = 2;
                    }
                    if ui.button("Save as PDF…").clicked() {
                        action = 3;
                    }
                });
            });
        self.s.paper = paper;
        if !open {
            self.print = None;
            return;
        }
        match action {
            1 => {
                if let Some(path) = self.temp_pdf(ctx) {
                    let d = self.print.as_ref().unwrap();
                    let printer = d.printer.clone().unwrap_or_default();
                    let copies = d.copies;
                    let res = crate::pdf::send_to_printer(&printer, copies, self.s.paper, &path);
                    let d = self.print.as_mut().unwrap();
                    d.status = Some(match res {
                        Ok(_) => (true, format!("Sent {pages} page{} to {}", if pages == 1 { "" } else { "s" }, printer.replace('_', " "))),
                        Err(e) => (false, format!("The printer didn't accept it: {e}")),
                    });
                }
            }
            2 => {
                if let Some(path) = self.temp_pdf(ctx) {
                    let _ = std::process::Command::new("open").arg("-a").arg("Preview").arg(&path).spawn();
                }
            }
            3 => self.save_pdf(ctx),
            _ => {}
        }
    }

    fn shortcuts_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_shortcuts;
        egui::Window::new("Keyboard shortcuts").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
            let cmd = if cfg!(target_os = "macos") { "⌘" } else { "Ctrl+" };
            egui::Grid::new("keys").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                let rows = [
                    ("Space".to_string(), "Play / pause"),
                    ("← →".to_string(), "Previous / next bar"),
                    ("↑ ↓".to_string(), "Previous / next track"),
                    ("Home".to_string(), "Back to the start"),
                    (format!("{cmd}O"), "Open a file"),
                    (format!("{cmd}P"), "Print"),
                    (format!("{cmd}I"), "Song information"),
                    (format!("{cmd}E"), "Export as MIDI"),
                    (format!("{cmd}1 / 2 / 3"), "Notation and tab / tab only / notation only"),
                    (format!("{cmd}= / {cmd}− / {cmd}0"), "Zoom in / out / actual size"),
                    ("1 – 5".to_string(), "Rate the open file (0 clears)"),
                    ("Click a note".to_string(), "Start playing from there"),
                    ("Shift-click".to_string(), "Select bars to loop"),
                ];
                for (k, v) in rows {
                    ui.label(egui::RichText::new(k).monospace().strong());
                    ui.label(v);
                    ui.end_row();
                }
            });
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Tip: drop a .gp3 / .gp4 file on the window to open it.").weak());
        });
        self.show_shortcuts = open;
    }

    #[cfg(target_os = "macos")]
    fn mac_menu_frame(&mut self, ctx: &egui::Context) {
        use crate::macmenu::{Action, MenuState};
        let actions = self.mac_menu.as_ref().map(|m| m.poll()).unwrap_or_default();
        for a in actions {
            match a {
                Action::Open => self.open_dialog(ctx),
                Action::ChooseLibrary => self.choose_library(ctx),
                Action::Recent(i) => {
                    if let Some(p) = self.s.recent.get(i).cloned() {
                        self.open(&p, ctx);
                    }
                }
                Action::Print => self.open_print(),
                Action::SavePdf => {
                    self.open_print();
                    self.save_pdf(ctx);
                }
                Action::ExportMidi => self.export_midi(),
                Action::SongInfo => self.show_info = true,
                Action::ViewBoth => self.s.mode = ViewMode::Both,
                Action::ViewTab => self.s.mode = ViewMode::Tab,
                Action::ViewNotation => self.s.mode = ViewMode::Notation,
                Action::AllTracks => self.all_tracks = !self.all_tracks,
                Action::Library => self.s.show_library = !self.s.show_library,
                Action::Mixer => self.s.show_mixer = !self.s.show_mixer,
                Action::Fretboard => self.s.show_fretboard = !self.s.show_fretboard,
                Action::Piano => self.s.show_piano = !self.s.show_piano,
                Action::ZoomIn => self.s.zoom = (self.s.zoom + 0.1).min(2.5),
                Action::ZoomOut => self.s.zoom = (self.s.zoom - 0.1).max(0.6),
                Action::ZoomReset => self.s.zoom = 1.0,
                Action::ThemeSystem | Action::ThemeLight | Action::ThemeDark => {
                    self.s.theme = match a {
                        Action::ThemeLight => ThemeChoice::Light,
                        Action::ThemeDark => ThemeChoice::Dark,
                        _ => ThemeChoice::System,
                    };
                    self.style(ctx);
                }
                Action::PlayPause => self.play_pause(),
                Action::BackToStart => self.stop(),
                Action::PrevBar => self.move_bar(-1),
                Action::NextBar => self.move_bar(1),
                Action::PrevTrack => self.track = self.track.saturating_sub(1),
                Action::NextTrack => {
                    if self.track + 1 < self.song.tracks.len() {
                        self.track += 1;
                    }
                }
                Action::Metronome => {
                    self.s.metronome = !self.s.metronome;
                    self.apply_player_settings();
                }
                Action::CountIn => {
                    self.s.count_in = !self.s.count_in;
                    self.apply_player_settings();
                }
                Action::Follow => self.s.follow = !self.s.follow,
                Action::Loop => {
                    self.looping = !self.looping;
                    self.update_loop();
                }
                Action::Realistic => {
                    self.s.simple_sound = !self.s.simple_sound;
                    self.apply_player_settings();
                }
                Action::Shortcuts => self.show_shortcuts = true,
            }
        }
        let st = MenuState {
            mode: match self.s.mode {
                ViewMode::Both => 0,
                ViewMode::Tab => 1,
                ViewMode::Notation => 2,
            },
            all_tracks: self.all_tracks,
            library: self.s.show_library,
            mixer: self.s.show_mixer,
            fretboard: self.s.show_fretboard,
            piano: self.s.show_piano,
            theme: match self.s.theme {
                ThemeChoice::System => 0,
                ThemeChoice::Light => 1,
                ThemeChoice::Dark => 2,
            },
            metronome: self.s.metronome,
            count_in: self.s.count_in,
            follow: self.s.follow,
            looping: self.looping,
            realistic: !self.s.simple_sound,
            playing: self.player.is_playing(),
            has_file: self.path.is_some(),
        };
        let recent = self.s.recent.clone();
        if let Some(m) = &mut self.mac_menu {
            m.sync(&st, &recent);
        }
    }

    fn info_window(&mut self, ctx: &egui::Context) {
        let mut open = self.show_info;
        let song = self.song.clone();
        egui::Window::new("Song information").open(&mut open).default_width(420.0).show(ctx, |ui| {
            egui::Grid::new("info").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                let row = |ui: &mut egui::Ui, k: &str, v: &str| {
                    if !v.is_empty() {
                        ui.label(egui::RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    }
                };
                row(ui, "Title", &song.title);
                row(ui, "Subtitle", &song.subtitle);
                row(ui, "Artist", &song.artist);
                row(ui, "Album", &song.album);
                row(ui, "Words & music", &song.words);
                row(ui, "Copyright", &song.copyright);
                row(ui, "Tabbed by", &song.tab_author);
                row(ui, "Instructions", &song.instructions);
                row(ui, "Tempo", &song.tempo.to_string());
                row(ui, "Bars", &song.headers.len().to_string());
                row(ui, "Tracks", &song.tracks.len().to_string());
                if let Some(p) = &self.path {
                    row(ui, "File", &p.to_string_lossy());
                }
                row(ui, "Format", song.version.trim());
            });
            let notes: Vec<&String> = song.notice.iter().filter(|n| !n.trim().is_empty()).collect();
            if !notes.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("Notes").strong());
                for n in notes {
                    ui.label(n);
                }
            }
            let lyrics: Vec<&(i32, String)> = song.lyrics.iter().filter(|l| !l.1.trim().is_empty()).collect();
            if !lyrics.is_empty() {
                ui.separator();
                ui.label(egui::RichText::new("Lyrics in this file").strong());
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for l in lyrics {
                        ui.label(&l.1);
                    }
                });
            }
        });
        self.show_info = open;
    }
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.screenshot.is_none() {
            eframe::set_value(storage, "settings", &self.s);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        score::set_dark(ctx.theme() == egui::Theme::Dark);
        // dropped files
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if let Some(p) = dropped.into_iter().find(|p| is_tab_file(p)) {
            self.open(&p, &ctx);
        }
        self.keys(&ctx);

        #[cfg(target_os = "macos")]
        self.mac_menu_frame(&ctx);
        #[cfg(not(target_os = "macos"))]
        egui::Panel::top("menu").show(ui, |ui| {
            self.menu(ui);
        });
        egui::Panel::top("toolbar").frame(egui::Frame::side_top_panel(&ctx.global_style()).inner_margin(6.0)).show(ui, |ui| {
            self.toolbar(ui);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some(e) = &self.error {
                    ui.colored_label(Color32::from_rgb(200, 60, 50), e);
                    if ui.small_button("OK").clicked() {
                        self.error = None;
                    }
                } else {
                    let name = self
                        .path
                        .as_ref()
                        .and_then(|p| p.file_name())
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "Sample piece".into());
                    ui.label(egui::RichText::new(name).small());
                    ui.label(egui::RichText::new(format!("· {} tracks · {} bars · tempo {}", self.song.tracks.len(), self.song.headers.len(), self.song.tempo)).small().weak());
                }
                if let Some(e) = &self.player.error {
                    ui.colored_label(Color32::from_rgb(200, 120, 40), format!("Sound is off: {e}"));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Song info").clicked() {
                        self.show_info = true;
                    }
                    if let Some(path) = self.path.clone() {
                        ui.separator();
                        let current = self.rating_of(&path);
                        if let Some(r) = star_picker(ui, current, 15.0) {
                            self.set_rating(&path, r);
                        }
                        ui.label(egui::RichText::new("Your rating").small().weak());
                    }
                });
            });
        });
        if self.s.show_fretboard {
            egui::Panel::bottom("fretboard").exact_size(170.0).frame(egui::Frame::NONE).show(ui, |ui| {
                self.fretboard(ui);
            });
        }
        if self.s.show_piano {
            egui::Panel::bottom("piano").exact_size(110.0).frame(egui::Frame::NONE).show(ui, |ui| {
                self.piano(ui);
            });
        }
        if self.s.show_library {
            egui::Panel::left("library").resizable(true).default_size(270.0).min_size(180.0).show(ui, |ui| {
                self.library_panel(ui);
            });
        }
        if self.s.show_mixer {
            egui::Panel::right("tracks").resizable(true).default_size(240.0).min_size(180.0).show(ui, |ui| {
                self.mixer_panel(ui);
            });
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(if ctx.theme() == egui::Theme::Dark { Color32::from_gray(22) } else { Color32::from_rgb(214, 217, 222) }))
            .show(ui, |ui| {
                self.score_view(ui);
            });
        if self.show_info {
            self.info_window(&ctx);
        }
        if self.print.is_some() {
            self.print_window(&ctx);
        }
        if self.show_shortcuts {
            self.shortcuts_window(&ctx);
        }

        if self.player.is_playing() || self.lib_scanning {
            ctx.request_repaint();
        }

        // --screenshot
        if let Some(path) = self.screenshot.clone() {
            self.frames += 1;
            ctx.request_repaint();
            if self.frames == 60 {
                // automated check of the printed pages
                if let Ok(out) = std::env::var("FRETBOOK_PDF") {
                    self.open_print();
                    if let Some(d) = &mut self.print {
                        d.all_tracks = self.all_tracks;
                    }
                    let bytes = crate::pdf::make_pdf(&ctx, &self.print_job());
                    let _ = std::fs::write(out, bytes);
                }
            }
            if self.frames == 120 {
                let done = self.shot_done.clone();
                let ctx2 = ctx.clone();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |img| {
                    let [w, h] = img.size;
                    let mut buf = Vec::with_capacity(w * h * 4);
                    for c in &img.pixels {
                        buf.extend_from_slice(&c.to_array());
                    }
                    if let Some(parent) = path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if let Err(e) = image::save_buffer(&path, &buf, w as u32, h as u32, image::ColorType::Rgba8) {
                        eprintln!("could not save screenshot: {e}");
                    }
                    done.store(true, std::sync::atomic::Ordering::SeqCst);
                    ctx2.request_repaint();
                })));
            }
            if self.shot_done.load(std::sync::atomic::Ordering::SeqCst) || self.frames > 600 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Icon {
    Start,
    Prev,
    Play,
    Pause,
    Next,
    Sun,
    Moon,
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, primary: bool) -> egui::Response {
    let size = if primary { Vec2::new(46.0, 28.0) } else { Vec2::new(34.0, 28.0) };
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let vis = ui.style().interact(&resp);
    let fill = if primary {
        if resp.hovered() { Color32::from_rgb(70, 138, 235) } else { Color32::from_rgb(52, 120, 220) }
    } else {
        vis.weak_bg_fill
    };
    let p = ui.painter();
    p.rect_filled(rect, 5.0, fill);
    let col = if primary { Color32::WHITE } else { vis.fg_stroke.color };
    let c = rect.center();
    let tri = |dx: f32, dir: f32, w: f32| {
        vec![
            Pos2::new(c.x + dx - w * dir, c.y - 6.5),
            Pos2::new(c.x + dx + w * dir, c.y),
            Pos2::new(c.x + dx - w * dir, c.y + 6.5),
        ]
    };
    match icon {
        Icon::Play => {
            p.add(egui::Shape::convex_polygon(tri(1.5, 1.0, 6.0), col, Stroke::NONE));
        }
        Icon::Pause => {
            p.rect_filled(Rect::from_center_size(c - Vec2::new(4.0, 0.0), Vec2::new(4.0, 13.0)), 1.0, col);
            p.rect_filled(Rect::from_center_size(c + Vec2::new(4.0, 0.0), Vec2::new(4.0, 13.0)), 1.0, col);
        }
        Icon::Start => {
            p.rect_filled(Rect::from_center_size(c - Vec2::new(6.0, 0.0), Vec2::new(2.5, 13.0)), 0.0, col);
            p.add(egui::Shape::convex_polygon(tri(1.5, -1.0, 5.5), col, Stroke::NONE));
        }
        Icon::Prev => {
            p.add(egui::Shape::convex_polygon(tri(-4.0, -1.0, 4.5), col, Stroke::NONE));
            p.add(egui::Shape::convex_polygon(tri(4.0, -1.0, 4.5), col, Stroke::NONE));
        }
        Icon::Sun => {
            p.circle_filled(c, 4.5, col);
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let d = Vec2::new(a.cos(), a.sin());
                p.line_segment([c + d * 6.5, c + d * 9.0], Stroke::new(1.6, col));
            }
        }
        Icon::Moon => {
            p.circle_filled(c, 7.0, col);
            p.circle_filled(c + Vec2::new(3.5, -2.5), 6.0, fill);
        }
        Icon::Next => {
            p.add(egui::Shape::convex_polygon(tri(-4.0, 1.0, 4.5), col, Stroke::NONE));
            p.add(egui::Shape::convex_polygon(tri(4.0, 1.0, 4.5), col, Stroke::NONE));
        }
    }
    resp
}

/// Fonts: the Mac's own Helvetica Neue for the window, Times New Roman for the
/// printed score, Menlo for numbers that tick. egui's built-in fonts stay as a
/// fallback for any missing characters (and if a font file isn't there).
fn build_fonts(cjk: bool) -> egui::FontDefinitions {
    use egui::FontFamily;
    let mut fonts = egui::FontDefinitions::default();
    let defaults_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let defaults_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let mut load = |key: &str, paths: &[&str], index: u32| -> Option<String> {
        for p in paths {
            if let Ok(bytes) = std::fs::read(p) {
                let mut fd = egui::FontData::from_owned(bytes);
                fd.index = index;
                fonts.font_data.insert(key.to_string(), Arc::new(fd));
                return Some(key.to_string());
            }
        }
        None
    };
    let ui = load("ui", &["/System/Library/Fonts/HelveticaNeue.ttc", "/System/Library/Fonts/Helvetica.ttc"], 0);
    let mono = load("mono", &["/System/Library/Fonts/Menlo.ttc"], 0);
    let serif = load("serif", &["/System/Library/Fonts/Supplemental/Times New Roman.ttf"], 0);
    let serif_bold = load("serif-bold", &["/System/Library/Fonts/Supplemental/Times New Roman Bold.ttf"], 0);
    let wide = if cjk {
        load(
            "wide",
            &[
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
                "/Library/Fonts/Arial Unicode.ttf",
                "/System/Library/Fonts/Hiragino Sans GB.ttc",
            ],
            0,
        )
    } else {
        None
    };
    let family = |first: &[&Option<String>], rest: &[String]| -> Vec<String> {
        let mut v: Vec<String> = first.iter().filter_map(|x| (*x).clone()).collect();
        v.extend(rest.iter().cloned());
        if let Some(w) = &wide {
            v.push(w.clone());
        }
        v
    };
    let prop = family(&[&ui], &defaults_prop);
    let monos = family(&[&mono], &defaults_mono);
    let score = family(&[&serif, &ui], &defaults_prop);
    let score_bold = family(&[&serif_bold, &serif, &ui], &defaults_prop);
    fonts.families.insert(FontFamily::Proportional, prop);
    fonts.families.insert(FontFamily::Monospace, monos);
    fonts.families.insert(FontFamily::Name("score".into()), score);
    fonts.families.insert(FontFamily::Name("score-bold".into()), score_bold);
    fonts
}


/// Five stars, the first `filled` of them gold.
fn draw_stars(p: &egui::Painter, min: Pos2, size: f32, filled: u8, preview: bool) {
    let gold = if preview { Color32::from_rgb(255, 196, 70) } else { Color32::from_rgb(240, 175, 30) };
    let empty = Color32::from_gray(150);
    for i in 0..5u8 {
        let c = Pos2::new(min.x + size * (i as f32 + 0.5), min.y + size * 0.55);
        star(p, c, size * 0.48, if i < filled { Some(gold) } else { None }, empty);
    }
}

fn star(p: &egui::Painter, c: Pos2, r: f32, fill: Option<Color32>, outline: Color32) {
    let pts: Vec<Pos2> = (0..10)
        .map(|k| {
            let a = -std::f32::consts::FRAC_PI_2 + k as f32 * std::f32::consts::PI / 5.0;
            let rr = if k % 2 == 0 { r } else { r * 0.45 };
            Pos2::new(c.x + rr * a.cos(), c.y + rr * a.sin())
        })
        .collect();
    if let Some(col) = fill {
        // a star is drawn as a fan of triangles around its centre
        let mut mesh = egui::Mesh::default();
        mesh.colored_vertex(c, col);
        for q in &pts {
            mesh.colored_vertex(*q, col);
        }
        for k in 0..10u32 {
            mesh.add_triangle(0, 1 + k, 1 + (k + 1) % 10);
        }
        p.add(egui::Shape::mesh(mesh));
    } else {
        p.add(egui::Shape::closed_line(pts, Stroke::new(1.0, outline)));
    }
}

/// Clickable row of five stars; returns a new rating when clicked.
fn star_picker(ui: &mut egui::Ui, current: u8, size: f32) -> Option<u8> {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(size * 5.0, size + 2.0), Sense::click());
    let hover = resp.hover_pos().map(|p| (((p.x - rect.min.x) / size).floor() as i32 + 1).clamp(1, 5) as u8);
    draw_stars(ui.painter(), rect.min, size, hover.unwrap_or(current), hover.is_some());
    let resp = resp.on_hover_text("Rate this file (1–5 keys work too; click the same star again to clear)");
    if resp.clicked() {
        let v = hover.unwrap_or(0);
        return Some(if v == current { 0 } else { v });
    }
    None
}
