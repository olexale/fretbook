//! The real macOS menu bar (at the top of the screen) for the app.

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{AboutMetadata, CheckMenuItem, IsMenuItem, Menu, MenuId, MenuItem, PredefinedMenuItem, Submenu};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Action {
    Open,
    ChooseLibrary,
    Recent(usize),
    Print,
    SavePdf,
    ExportMidi,
    SongInfo,
    ViewBoth,
    ViewTab,
    ViewNotation,
    AllTracks,
    Library,
    Mixer,
    Fretboard,
    Piano,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ThemeSystem,
    ThemeLight,
    ThemeDark,
    PlayPause,
    BackToStart,
    PrevBar,
    NextBar,
    PrevTrack,
    NextTrack,
    Metronome,
    CountIn,
    Follow,
    Loop,
    Realistic,
    Shortcuts,
}

/// What the check marks in the menus should show.
#[derive(Clone, PartialEq)]
pub struct MenuState {
    pub mode: u8, // 0 both, 1 tab, 2 notation
    pub all_tracks: bool,
    pub library: bool,
    pub mixer: bool,
    pub fretboard: bool,
    pub piano: bool,
    pub theme: u8, // 0 system, 1 light, 2 dark
    pub metronome: bool,
    pub count_in: bool,
    pub follow: bool,
    pub looping: bool,
    pub realistic: bool,
    pub playing: bool,
    pub has_file: bool,
}

pub struct MacMenu {
    _menu: Menu,
    ids: HashMap<MenuId, Action>,
    checks: Vec<(Action, CheckMenuItem)>,
    play: MenuItem,
    recent: Submenu,
    recent_items: Vec<MenuItem>,
    recent_shown: Vec<PathBuf>,
    rx: Receiver<MenuId>,
    last: Option<MenuState>,
}

fn cmd(code: Code) -> Option<Accelerator> {
    Some(Accelerator::new(Modifiers::META, code))
}

impl MacMenu {
    pub fn new(ctx: &eframe::egui::Context) -> MacMenu {
        let (tx, rx) = channel();
        let ctx2 = ctx.clone();
        let tx = std::sync::Mutex::new(tx);
        muda::MenuEvent::set_event_handler(Some(move |e: muda::MenuEvent| {
            if let Ok(t) = tx.lock() {
                let _ = t.send(e.id);
            }
            ctx2.request_repaint();
        }));

        let mut ids = HashMap::new();
        let mut checks = Vec::new();
        let mut item = |text: &str, action: Action, acc: Option<Accelerator>| -> MenuItem {
            let m = MenuItem::new(text, true, acc);
            ids.insert(m.id().clone(), action);
            m
        };
        let mut check_ids: Vec<(MenuId, Action)> = Vec::new();
        let mut check = |text: &str, action: Action, acc: Option<Accelerator>| -> CheckMenuItem {
            let m = CheckMenuItem::new(text, true, false, acc);
            check_ids.push((m.id().clone(), action));
            checks.push((action, m.clone()));
            m
        };
        let sep = PredefinedMenuItem::separator;

        // App menu
        let app_menu = Submenu::with_items(
            "Fretbook",
            true,
            &[
                &PredefinedMenuItem::about(
                    Some("About Fretbook"),
                    Some(AboutMetadata {
                        name: Some("Fretbook".into()),
                        version: Some(env!("CARGO_PKG_VERSION").into()),
                        comments: Some("Opens, shows and plays .gp3 / .gp4 tab files.\nSound bank: GeneralUser GS by S. Christian Collins.".into()),
                        ..Default::default()
                    }),
                ),
                &sep(),
                &PredefinedMenuItem::services(None),
                &sep(),
                &PredefinedMenuItem::hide(Some("Hide Fretbook")),
                &PredefinedMenuItem::hide_others(None),
                &PredefinedMenuItem::show_all(None),
                &sep(),
                &PredefinedMenuItem::quit(Some("Quit Fretbook")),
            ],
        )
        .expect("menu");

        // File
        let recent = Submenu::new("Open Recent", true);
        let file = Submenu::new("File", true);
        let _ = file.append_items(&[
            &item("Open…", Action::Open, cmd(Code::KeyO)),
            &recent,
            &item("Choose Library Folder…", Action::ChooseLibrary, None),
            &sep(),
            &item("Song Information", Action::SongInfo, cmd(Code::KeyI)),
            &sep(),
            &item("Save as PDF…", Action::SavePdf, None),
            &item("Export as MIDI…", Action::ExportMidi, cmd(Code::KeyE)),
            &sep(),
            &item("Print…", Action::Print, cmd(Code::KeyP)),
            &sep(),
            &PredefinedMenuItem::close_window(None),
        ]);

        // Edit (standard, so copy/paste works in the search box)
        let edit = Submenu::with_items(
            "Edit",
            true,
            &[
                &PredefinedMenuItem::undo(None),
                &PredefinedMenuItem::redo(None),
                &sep(),
                &PredefinedMenuItem::cut(None),
                &PredefinedMenuItem::copy(None),
                &PredefinedMenuItem::paste(None),
                &PredefinedMenuItem::select_all(None),
            ],
        )
        .expect("menu");

        // View
        let appearance = Submenu::new("Appearance", true);
        let _ = appearance.append_items(&[
            &check("Same as My Mac", Action::ThemeSystem, None),
            &check("Light", Action::ThemeLight, None),
            &check("Dark", Action::ThemeDark, None),
        ]);
        let view = Submenu::new("View", true);
        let _ = view.append_items(&[
            &check("Notation and Tab", Action::ViewBoth, cmd(Code::Digit1)),
            &check("Tab Only", Action::ViewTab, cmd(Code::Digit2)),
            &check("Notation Only", Action::ViewNotation, cmd(Code::Digit3)),
            &sep(),
            &check("Show All Tracks Together", Action::AllTracks, None),
            &item("Previous Track", Action::PrevTrack, None),
            &item("Next Track", Action::NextTrack, None),
            &sep(),
            &check("Library", Action::Library, Some(Accelerator::new(Modifiers::META | Modifiers::ALT, Code::KeyL))),
            &check("Tracks Panel", Action::Mixer, Some(Accelerator::new(Modifiers::META | Modifiers::ALT, Code::KeyT))),
            &check("Fretboard", Action::Fretboard, Some(Accelerator::new(Modifiers::META | Modifiers::ALT, Code::KeyF))),
            &check("Piano", Action::Piano, Some(Accelerator::new(Modifiers::META | Modifiers::ALT, Code::KeyK))),
            &sep(),
            &item("Zoom In", Action::ZoomIn, cmd(Code::Equal)),
            &item("Zoom Out", Action::ZoomOut, cmd(Code::Minus)),
            &item("Actual Size", Action::ZoomReset, cmd(Code::Digit0)),
            &sep(),
            &appearance,
            &sep(),
            &PredefinedMenuItem::fullscreen(None),
        ]);

        // Playback
        let play = item("Play", Action::PlayPause, None);
        let playback = Submenu::new("Playback", true);
        let _ = playback.append_items(&[
            &play,
            &item("Back to Start", Action::BackToStart, None),
            &item("Previous Bar", Action::PrevBar, None),
            &item("Next Bar", Action::NextBar, None),
            &sep(),
            &check("Metronome", Action::Metronome, None),
            &check("Count In", Action::CountIn, None),
            &check("Follow the Cursor", Action::Follow, None),
            &check("Loop the Selected Bars", Action::Loop, None),
            &sep(),
            &check("Realistic Instruments", Action::Realistic, None),
        ]);

        // Window and Help
        let window = Submenu::with_items(
            "Window",
            true,
            &[&PredefinedMenuItem::minimize(None), &PredefinedMenuItem::maximize(Some("Zoom"))],
        )
        .expect("menu");
        let help = Submenu::new("Help", true);
        let _ = help.append(&item("Keyboard Shortcuts", Action::Shortcuts, None));

        for (id, a) in check_ids {
            ids.insert(id, a);
        }

        let menu = Menu::new();
        let tops: [&dyn IsMenuItem; 7] = [&app_menu, &file, &edit, &view, &playback, &window, &help];
        let _ = menu.append_items(&tops);
        menu.init_for_nsapp();
        // automated check: write the menu structure
        if let Ok(path) = std::env::var("FRETBOOK_MENU_DUMP") {
            let mut out = String::new();
            for top in menu.items() {
                if let Some(sm) = top.as_submenu() {
                    out.push_str(&format!("{}:", sm.text()));
                    for it in sm.items() {
                        let t = match &it {
                            muda::MenuItemKind::MenuItem(m) => m.text(),
                            muda::MenuItemKind::Check(c) => c.text(),
                            muda::MenuItemKind::Submenu(s) => format!("{} >", s.text()),
                            muda::MenuItemKind::Predefined(p) => p.text(),
                            _ => String::new(),
                        };
                        if !t.is_empty() {
                            out.push_str(&format!(" [{t}]"));
                        }
                    }
                    out.push('\n');
                }
            }
            let _ = std::fs::write(path, out);
        }
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();

        MacMenu {
            _menu: menu,
            ids,
            checks,
            play,
            recent,
            recent_items: Vec::new(),
            recent_shown: vec![PathBuf::from("\u{0}")],
            rx,
            last: None,
        }
    }

    pub fn poll(&self) -> Vec<Action> {
        let mut out = Vec::new();
        while let Ok(id) = self.rx.try_recv() {
            if let Some(a) = self.ids.get(&id) {
                out.push(*a);
            }
        }
        out
    }

    /// Updates check marks, the Play/Pause label and the recent files.
    pub fn sync(&mut self, st: &MenuState, recent: &[PathBuf]) {
        if self.last.as_ref() != Some(st) {
            for (a, item) in &self.checks {
                let on = match a {
                    Action::ViewBoth => st.mode == 0,
                    Action::ViewTab => st.mode == 1,
                    Action::ViewNotation => st.mode == 2,
                    Action::AllTracks => st.all_tracks,
                    Action::Library => st.library,
                    Action::Mixer => st.mixer,
                    Action::Fretboard => st.fretboard,
                    Action::Piano => st.piano,
                    Action::ThemeSystem => st.theme == 0,
                    Action::ThemeLight => st.theme == 1,
                    Action::ThemeDark => st.theme == 2,
                    Action::Metronome => st.metronome,
                    Action::CountIn => st.count_in,
                    Action::Follow => st.follow,
                    Action::Loop => st.looping,
                    Action::Realistic => st.realistic,
                    _ => false,
                };
                item.set_checked(on);
            }
            self.play.set_text(if st.playing { "Pause" } else { "Play" });
            self.last = Some(st.clone());
        }
        if self.recent_shown.as_slice() != recent {
            for it in self.recent_items.drain(..) {
                self.ids.remove(it.id());
                let _ = self.recent.remove(&it);
            }
            if recent.is_empty() {
                let it = MenuItem::new("Nothing Yet", false, None);
                let _ = self.recent.append(&it);
                self.recent_items.push(it);
            }
            for (i, p) in recent.iter().enumerate() {
                let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
                let it = MenuItem::new(name, true, None);
                self.ids.insert(it.id().clone(), Action::Recent(i));
                let _ = self.recent.append(&it);
                self.recent_items.push(it);
            }
            self.recent_shown = recent.to_vec();
        }
    }
}
