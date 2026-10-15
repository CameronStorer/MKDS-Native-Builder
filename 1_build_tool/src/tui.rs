//! Terminal menu for players: play the built game, build it from a ROM, change the controls.
use crate::build;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, ModifierKeyCode};
use mkds_builder::controls::{self, ACTIONS, Controls, KEYS, ROLES};
use mkds_builder::rom::{RomInfo, inspect_rom};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

pub struct Config {
    pub source: PathBuf,
    /// The game folder (`builds/mkds` in the repository unless `--out` says otherwise).
    pub output: PathBuf,
    pub offline: bool,
    pub allow_unverified: bool,
    pub initial_rom: Option<PathBuf>,
}

enum Msg {
    Log(String),
    Done(Result<PathBuf, String>),
}

enum Screen {
    Home { list: ListState, note: Option<(String, Color)> },
    Rom { input: String, error: Option<String> },
    Building { log: Vec<String>, rx: Receiver<Msg>, result: Option<Result<PathBuf, String>> },
    Controls { c: Controls, path: Option<PathBuf>, list: ListState, capture: bool, note: Option<(String, Color)> },
}

enum Exit {
    Quit,
    Play,
}

/// Runs the menu until the player quits; "Play" leaves the menu while the game runs, then comes back.
pub fn run(mut cfg: Config) -> std::io::Result<()> {
    let mut note = None;
    loop {
        let mut terminal = ratatui::init();
        let exit = app(&mut terminal, &mut cfg, note.take());
        ratatui::restore();
        match exit? {
            Exit::Quit => return Ok(()),
            Exit::Play => {
                println!("Starting the game (close its window or press your Quit key to come back here)...");
                note = Some(match build::launch(&cfg.output, &[]) {
                    Ok(0) => ("The game was closed.".to_string(), Color::Green),
                    Ok(code) => (format!("The game stopped with code {code}; see the messages above it."), Color::Yellow),
                    Err(e) => (format!("Could not start the game: {e}"), Color::Red),
                });
            }
        }
    }
}

fn built(cfg: &Config) -> bool {
    cfg.output.join("manifest.json").is_file()
}

fn home_items(cfg: &Config) -> Vec<&'static str> {
    let mut v = Vec::new();
    if built(cfg) {
        v.push("Play");
        v.push("Rebuild the game from my ROM");
    } else {
        v.push("Build the game from my ROM");
    }
    v.extend(["Change controls", "Quit"]);
    v
}

fn home(note: Option<(String, Color)>) -> Screen {
    Screen::Home { list: ListState::default().with_selected(Some(0)), note }
}

fn app(terminal: &mut DefaultTerminal, cfg: &mut Config, note: Option<(String, Color)>) -> std::io::Result<Exit> {
    let mut screen = match cfg.initial_rom.take() {
        Some(rom) => Screen::Rom { input: rom.to_string_lossy().into_owned(), error: None },
        None => home(note),
    };
    loop {
        terminal.draw(|f| draw(f, &screen, cfg))?;
        if let Screen::Building { log, rx, result } = &mut screen {
            while let Ok(m) = rx.try_recv() {
                match m {
                    Msg::Log(l) => log.push(l),
                    Msg::Done(r) => *result = Some(r),
                }
            }
        }
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let capturing = matches!(screen, Screen::Controls { capture: true, .. });
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') && !capturing {
            if matches!(screen, Screen::Building { result: None, .. }) {
                continue; // a build is running: finish it rather than leave a half-written folder
            }
            return Ok(Exit::Quit);
        }
        screen = match screen {
            Screen::Home { mut list, note } => {
                let items = home_items(cfg);
                let sel = list.selected().unwrap_or(0).min(items.len() - 1);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(Exit::Quit),
                    KeyCode::Up => list.select(Some((sel + items.len() - 1) % items.len())),
                    KeyCode::Down => list.select(Some((sel + 1) % items.len())),
                    KeyCode::Enter => match items[sel] {
                        "Play" => return Ok(Exit::Play),
                        "Change controls" => {
                            let path = controls::default_path();
                            let (c, note) = match path.as_deref().map(Controls::load_or_create) {
                                Some(Ok(c)) => (c, None),
                                Some(Err(e)) => (Controls::default(), Some((format!("{e}. Showing the defaults; saving replaces the file."), Color::Red))),
                                None => (Controls::default(), Some(("No settings folder found; changes cannot be saved.".into(), Color::Red))),
                            };
                            screen = Screen::Controls { c, path, list: ListState::default().with_selected(Some(0)), capture: false, note };
                            continue;
                        }
                        "Quit" => return Ok(Exit::Quit),
                        _ => {
                            screen = Screen::Rom { input: String::new(), error: None };
                            continue;
                        }
                    },
                    _ => {}
                }
                Screen::Home { list, note }
            }
            Screen::Rom { mut input, error } => match key.code {
                KeyCode::Esc => home(None),
                KeyCode::Char(c) => {
                    input.push(c);
                    Screen::Rom { input, error: None }
                }
                KeyCode::Backspace => {
                    input.pop();
                    Screen::Rom { input, error: None }
                }
                KeyCode::Enter => match inspect_rom(&PathBuf::from(input.trim().trim_matches('"'))) {
                    Ok(info) if !info.reference_match && !cfg.allow_unverified => Screen::Rom {
                        input,
                        error: Some(format!("This ROM is not a clean copy of the supported version ({}).", info.compatibility)),
                    },
                    Ok(info) => start_build(info, cfg),
                    Err(e) => Screen::Rom { input, error: Some(e.to_string()) },
                },
                _ => Screen::Rom { input, error },
            },
            Screen::Building { log, rx, result } => match (&result, key.code) {
                (Some(Ok(_)), KeyCode::Enter | KeyCode::Esc) => home(Some(("The game is built. Choose Play.".into(), Color::Green))),
                (Some(Err(_)), KeyCode::Enter | KeyCode::Esc) => home(Some(("The build failed; your previous game (if any) was kept.".into(), Color::Red))),
                _ => Screen::Building { log, rx, result },
            },
            Screen::Controls { mut c, path, mut list, capture, note } => {
                let n = ACTIONS.len();
                let items = n + 3; // actions, then reset, save, back
                let sel = list.selected().unwrap_or(0);
                if capture {
                    let note = match key.code {
                        KeyCode::Esc => None,
                        code => match key_name(code) {
                            Some(k) => {
                                c.keys[sel] = vec![k];
                                Some((format!("{} = {}. Remember to save.", ACTIONS[sel], KEYS[k]), Color::Green))
                            }
                            None => Some(("That key can't be set here; edit controls.txt for it (path below).".into(), Color::Yellow)),
                        },
                    };
                    screen = Screen::Controls { c, path, list, capture: false, note };
                    continue;
                }
                match key.code {
                    KeyCode::Esc => {
                        screen = home(None);
                        continue;
                    }
                    KeyCode::Up => list.select(Some((sel + items - 1) % items)),
                    KeyCode::Down => list.select(Some((sel + 1) % items)),
                    KeyCode::Delete | KeyCode::Backspace if sel < n => {
                        c.keys[sel].clear();
                        screen = Screen::Controls { c, path, list, capture: false, note: Some((format!("{} has no key now.", ACTIONS[sel]), Color::Yellow)) };
                        continue;
                    }
                    KeyCode::Enter if sel < n => {
                        screen = Screen::Controls { c, path, list, capture: true, note: None };
                        continue;
                    }
                    KeyCode::Enter if sel == n => {
                        screen = Screen::Controls { c: Controls::default(), path, list, capture: false, note: Some(("Defaults restored. Remember to save.".into(), Color::Green)) };
                        continue;
                    }
                    KeyCode::Enter if sel == n + 1 => {
                        let saved = path.as_deref().ok_or_else(|| "No settings folder found.".to_string()).and_then(|p| c.save(p));
                        screen = match saved {
                            Ok(()) => home(Some(("Controls saved. They apply the next time the game starts.".into(), Color::Green))),
                            Err(e) => Screen::Controls { c, path, list, capture: false, note: Some((e, Color::Red)) },
                        };
                        continue;
                    }
                    KeyCode::Enter => {
                        screen = home(None);
                        continue;
                    }
                    _ => {}
                }
                Screen::Controls { c, path, list, capture, note }
            }
        };
    }
}

/// Builds into `<game>.building`, then swaps it in, so a failed build never removes a working game.
fn start_build(info: RomInfo, cfg: &Config) -> Screen {
    let (tx, rx) = channel();
    let (source, output, offline, allow_unverified) = (cfg.source.clone(), cfg.output.clone(), cfg.offline, cfg.allow_unverified);
    std::thread::spawn(move || {
        let tx2 = tx.clone();
        let mut log = |l| {
            let _ = tx2.send(Msg::Log(l));
        };
        let staging = sibling(&output, "building");
        let r = (|| -> Result<PathBuf, String> {
            if staging.exists() {
                std::fs::remove_dir_all(&staging).map_err(|e| format!("Cannot clear {}: {e}", staging.display()))?;
            }
            build::recompile(
                &info,
                &build::RecompileOptions {
                    source,
                    output: staging.clone(),
                    allow_unverified,
                    offline,
                    allow_incomplete: false,
                    keep_source: true,
                    with_acceptance: false,
                    jobs: None,
                    with_interpreter: false,
                    cache_dir: Some(build::default_cache_dir()),
                },
                &mut log,
            )
            .map_err(|e| e.to_string())?;
            replace_dir(&staging, &output)?;
            Ok(output)
        })();
        let _ = tx.send(Msg::Done(r));
    });
    Screen::Building { log: Vec::new(), rx, result: None }
}

fn sibling(dir: &Path, suffix: &str) -> PathBuf {
    let name = dir.file_name().map_or("mkds".into(), |n| n.to_string_lossy().into_owned());
    dir.with_file_name(format!("{name}.{suffix}"))
}

/// Moves `new` to `dest`; an existing `dest` must be a game folder (has `manifest.json`) and is deleted.
fn replace_dir(new: &Path, dest: &Path) -> Result<(), String> {
    let old = sibling(dest, "old");
    if dest.exists() {
        if !dest.join("manifest.json").is_file() {
            return Err(format!("{} exists but is not a game folder; not replacing it. The new build is in {}.", dest.display(), new.display()));
        }
        if old.exists() {
            std::fs::remove_dir_all(&old).map_err(|e| format!("Cannot remove {}: {e}", old.display()))?;
        }
        std::fs::rename(dest, &old).map_err(|e| format!("Cannot move the old game aside (is it still running?): {e}"))?;
    }
    std::fs::rename(new, dest).map_err(|e| format!("Cannot move the new game into {}: {e}", dest.display()))?;
    if old.exists() {
        let _ = std::fs::remove_dir_all(&old);
    }
    Ok(())
}

/// The controls-file name of a key pressed in the terminal (terminals cannot report Shift/Ctrl/Alt alone
/// on every system; those are set by editing the file).
fn key_name(code: KeyCode) -> Option<usize> {
    let name = match code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) if c.is_ascii_alphanumeric() => c.to_ascii_uppercase().to_string(),
        KeyCode::Char(c) => match c {
            '\'' => "Apostrophe", '`' => "Backquote", '\\' => "Backslash", ',' => "Comma", '=' => "Equal",
            '[' => "LeftBracket", '-' => "Minus", '.' => "Period", ']' => "RightBracket", ';' => "Semicolon",
            '/' => "Slash", _ => return None,
        }
        .to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Up => "Up".into(),
        KeyCode::Down => "Down".into(),
        KeyCode::Left => "Left".into(),
        KeyCode::Right => "Right".into(),
        KeyCode::Enter => "Enter".into(),
        KeyCode::Tab => "Tab".into(),
        KeyCode::Backspace => "Backspace".into(),
        KeyCode::Delete => "Delete".into(),
        KeyCode::Insert => "Insert".into(),
        KeyCode::Home => "Home".into(),
        KeyCode::End => "End".into(),
        KeyCode::PageUp => "PageUp".into(),
        KeyCode::PageDown => "PageDown".into(),
        KeyCode::CapsLock => "CapsLock".into(),
        KeyCode::ScrollLock => "ScrollLock".into(),
        KeyCode::NumLock => "NumLock".into(),
        KeyCode::Pause => "Pause".into(),
        KeyCode::Menu => "Menu".into(),
        KeyCode::Modifier(m) => match m {
            ModifierKeyCode::LeftShift => "LeftShift", ModifierKeyCode::RightShift => "RightShift",
            ModifierKeyCode::LeftControl => "LeftCtrl", ModifierKeyCode::RightControl => "RightCtrl",
            ModifierKeyCode::LeftAlt => "LeftAlt", ModifierKeyCode::RightAlt => "RightAlt",
            _ => return None,
        }
        .into(),
        _ => return None,
    };
    controls::key_index(&name)
}

fn draw(f: &mut Frame, screen: &Screen, cfg: &Config) {
    let [title, body, help] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(1)]).areas(f.area());
    f.render_widget(
        Paragraph::new("MKDS Native Builder").block(Block::bordered()).style(Style::new().add_modifier(Modifier::BOLD)),
        title,
    );
    let list_style = |l: List<'static>| l.highlight_symbol("> ").highlight_style(Style::new().fg(Color::Yellow));
    let noted = |v: &mut Vec<Line<'static>>, note: &Option<(String, Color)>| {
        if let Some((text, color)) = note {
            v.push(Line::styled(text.clone(), Style::new().fg(*color)));
        }
    };
    match screen {
        Screen::Home { list, note } => {
            let [top, bottom] = Layout::vertical([Constraint::Length(5), Constraint::Min(3)]).areas(body);
            let mut v = vec![if built(cfg) {
                Line::styled(format!("Game ready in {}", cfg.output.display()), Style::new().fg(Color::Green))
            } else {
                Line::from("No game built yet. Choose \"Build the game from my ROM\" (about 10-20 minutes the first time).")
            }];
            noted(&mut v, note);
            f.render_widget(Paragraph::new(v).wrap(Wrap { trim: false }).block(Block::bordered()), top);
            let items: Vec<ListItem> = home_items(cfg).into_iter().map(ListItem::new).collect();
            f.render_stateful_widget(list_style(List::new(items).block(Block::bordered())), bottom, &mut list.clone());
            f.render_widget(Paragraph::new("Up/Down: choose   Enter: select   Esc: quit"), help);
        }
        Screen::Rom { input, error } => {
            let mut v = vec![
                Line::from("Drag your MKDS .nds file (USA, revision 0) into this window, or type its path:"),
                Line::from(format!("> {input}_")),
            ];
            if let Some(e) = error {
                v.push(Line::styled(format!("Error: {e}"), Style::new().fg(Color::Red)));
            }
            f.render_widget(Paragraph::new(v).wrap(Wrap { trim: false }).block(Block::bordered()), body);
            f.render_widget(Paragraph::new("Enter: build   Esc: back"), help);
        }
        Screen::Building { log, result, .. } => {
            let rows = body.height.saturating_sub(3) as usize;
            let mut v: Vec<Line> = log.iter().skip(log.len().saturating_sub(rows)).map(|l| Line::from(l.as_str())).collect();
            v.push(match result {
                None => Line::styled("Building... (this window can stay open; it takes a while)", Style::new().fg(Color::Cyan)),
                Some(Ok(p)) => Line::styled(format!("Done: {}", p.display()), Style::new().fg(Color::Green)),
                Some(Err(e)) => Line::styled(format!("Failed: {e}"), Style::new().fg(Color::Red)),
            });
            f.render_widget(Paragraph::new(v).wrap(Wrap { trim: false }).block(Block::bordered()), body);
            f.render_widget(Paragraph::new(if result.is_some() { "Enter: back to the menu" } else { "Please wait" }), help);
        }
        Screen::Controls { c, path, list, capture, note } => {
            let [top, bottom] = Layout::vertical([Constraint::Min(5), Constraint::Length(4)]).areas(body);
            let mut items: Vec<ListItem> = (0..ACTIONS.len())
                .map(|a| {
                    let role = if ROLES[a].is_empty() { String::new() } else { format!("  ({})", ROLES[a]) };
                    ListItem::new(format!("{:<7} {:<16}{}", ACTIONS[a], c.describe(a), role))
                })
                .collect();
            items.extend(["Reset to defaults", "Save and go back", "Go back without saving"].map(ListItem::new));
            f.render_stateful_widget(
                list_style(List::new(items).block(Block::bordered().title("DS button   Keyboard key"))),
                top,
                &mut list.clone(),
            );
            let mut v = Vec::new();
            if *capture {
                let a = list.selected().unwrap_or(0);
                v.push(Line::styled(format!("Press the new key for {} (Esc: cancel)", ACTIONS[a]), Style::new().fg(Color::Cyan)));
            }
            noted(&mut v, note);
            v.push(Line::from(match path {
                Some(p) => format!("File: {}", p.display()),
                None => "File: (no settings folder)".into(),
            }));
            f.render_widget(Paragraph::new(v).wrap(Wrap { trim: false }), bottom);
            f.render_widget(Paragraph::new("Enter: change key   Delete: no key   Esc: back"), help);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_keys_map_to_controls_names() {
        let name = |c| key_name(c).map(|k| KEYS[k]);
        assert_eq!(name(KeyCode::Char('q')), Some("Q"));
        assert_eq!(name(KeyCode::Char('Q')), Some("Q"));
        assert_eq!(name(KeyCode::Char('7')), Some("Key7"));
        assert_eq!(name(KeyCode::Char(' ')), Some("Space"));
        assert_eq!(name(KeyCode::Char(';')), Some("Semicolon"));
        assert_eq!(name(KeyCode::Up), Some("Up"));
        assert_eq!(name(KeyCode::F(5)), Some("F5"));
        assert_eq!(name(KeyCode::Modifier(ModifierKeyCode::RightShift)), Some("RightShift"));
        assert_eq!(name(KeyCode::Char('!')), None);
        assert_eq!(name(KeyCode::F(20)), None);
    }

    #[test]
    fn every_screen_draws_on_a_small_terminal() {
        let cfg = Config { source: ".".into(), output: "no-such-game".into(), offline: true, allow_unverified: false, initial_rom: None };
        let note = Some(("note".to_string(), Color::Green));
        let screens = [
            home(note.clone()),
            Screen::Rom { input: "C:\\game.nds".into(), error: Some("Not found".into()) },
            Screen::Building { log: vec!["[1/5] Checking".into(); 50], rx: channel().1, result: Some(Err("x".into())) },
            Screen::Controls { c: Controls::default(), path: None, list: ListState::default().with_selected(Some(3)), capture: true, note },
        ];
        for (w, h) in [(80, 24), (40, 12)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            for s in &screens {
                t.draw(|f| draw(f, s, &cfg)).unwrap();
            }
        }
        let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        t.draw(|f| draw(f, &screens[3], &cfg)).unwrap();
        let text: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
        assert!(text.contains("L       A") && text.contains("use item"), "controls list shows L = A");
        assert!(text.contains("Press the new key for Start"));
    }

    #[test]
    fn a_rebuild_replaces_only_a_game_folder() {
        let dir = tempfile::tempdir().unwrap();
        let (new, dest) = (dir.path().join("mkds.building"), dir.path().join("mkds"));
        std::fs::create_dir(&new).unwrap();
        std::fs::write(new.join("manifest.json"), "new").unwrap();
        replace_dir(&new, &dest).unwrap(); // first build: nothing to replace
        std::fs::create_dir(&new).unwrap();
        std::fs::write(new.join("manifest.json"), "newer").unwrap();
        replace_dir(&new, &dest).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("manifest.json")).unwrap(), "newer");
        assert!(!new.exists() && !sibling(&dest, "old").exists());
        let other = dir.path().join("photos");
        std::fs::create_dir(&other).unwrap();
        std::fs::create_dir(&new).unwrap();
        assert!(replace_dir(&new, &other).is_err(), "a folder without manifest.json is never deleted");
        assert!(other.exists());
    }
}
