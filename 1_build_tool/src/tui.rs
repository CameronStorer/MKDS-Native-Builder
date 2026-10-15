//! Terminal UI: ROM path entry, validation summary, build with live log.
use crate::build::{self, Options};
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use mkds_builder::rom::{RomInfo, inspect_rom};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

enum Msg {
    Log(String),
    Done(Result<PathBuf, String>),
}

enum Screen {
    Path {
        input: String,
        error: Option<String>,
    },
    Menu {
        info: RomInfo,
        list: ListState,
    },
    Building {
        info: RomInfo,
        log: Vec<String>,
        rx: Receiver<Msg>,
        result: Option<Result<PathBuf, String>>,
        play: bool,
    },
}

pub struct Config {
    pub source: PathBuf,
    pub output: Option<PathBuf>,
    pub offline: bool,
    pub allow_unverified: bool,
    pub initial_rom: Option<PathBuf>,
}

/// Returns a bundle to launch (after the terminal is restored), if the user chose "Build and play".
pub fn run(cfg: Config) -> std::io::Result<Option<PathBuf>> {
    let mut terminal = ratatui::init();
    let result = app(&mut terminal, cfg);
    ratatui::restore();
    result
}

fn menu_items(info: &RomInfo, allow: bool) -> Vec<String> {
    let mut v = vec![
        "Build native game from ROM".to_string(),
        "Build native game from ROM and play".to_string(),
        "Build hand-port (legacy)".to_string(),
        "Build hand-port and play (legacy)".to_string(),
    ];
    if !info.reference_match {
        v.push(format!(
            "Experimental modified-ROM builds: {}",
            if allow { "ON" } else { "off" }
        ));
    }
    v.push("Choose another ROM".into());
    v.push("Quit".into());
    v
}

fn app(terminal: &mut DefaultTerminal, mut cfg: Config) -> std::io::Result<Option<PathBuf>> {
    let mut screen = Screen::Path {
        input: cfg
            .initial_rom
            .take()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        error: None,
    };
    let mut launch: Option<PathBuf> = None;
    loop {
        terminal.draw(|f| draw(f, &screen, &cfg))?;
        // Drain build messages.
        if let Screen::Building {
            log, rx, result, ..
        } = &mut screen
        {
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
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Ok(None);
        }
        screen = match screen {
            Screen::Path { mut input, error } => match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Char(c) => {
                    input.push(c);
                    Screen::Path { input, error: None }
                }
                KeyCode::Backspace => {
                    input.pop();
                    Screen::Path { input, error: None }
                }
                KeyCode::Enter => match inspect_rom(&PathBuf::from(input.trim().trim_matches('"')))
                {
                    Ok(info) => Screen::Menu {
                        info,
                        list: ListState::default().with_selected(Some(0)),
                    },
                    Err(e) => Screen::Path {
                        input,
                        error: Some(e.to_string()),
                    },
                },
                _ => Screen::Path { input, error },
            },
            Screen::Menu { info, mut list } => {
                let items = menu_items(&info, cfg.allow_unverified);
                let sel = list.selected().unwrap_or(0);
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => return Ok(None),
                    KeyCode::Up => list.select(Some((sel + items.len() - 1) % items.len())),
                    KeyCode::Down => list.select(Some((sel + 1) % items.len())),
                    KeyCode::Enter => {
                        let label = items[sel].as_str();
                        if label == "Quit" {
                            return Ok(None);
                        } else if label == "Choose another ROM" {
                            screen = Screen::Path {
                                input: String::new(),
                                error: None,
                            };
                            continue;
                        } else if label.starts_with("Experimental") {
                            cfg.allow_unverified = !cfg.allow_unverified;
                        } else if !info.reference_match && !cfg.allow_unverified {
                            // Refuse silently-unsafe builds; the toggle is explicit.
                            screen = Screen::Building {
                                info,
                                log: vec![
                                    "Modified ROM: enable the experimental option first.".into(),
                                ],
                                rx: channel().1,
                                result: Some(Err("Experimental option is off.".into())),
                                play: false,
                            };
                            continue;
                        } else {
                            let play = label.ends_with("and play") || label == "Build hand-port and play (legacy)";
                            let recomp = label.starts_with("Build native");
                            let (tx, rx) = channel();
                            let mut output = cfg
                                .output
                                .clone()
                                .unwrap_or_else(|| build::default_output(&info));
                            if recomp {
                                output = output.with_file_name(format!("{}-recompiled", &info.sha256[..16]));
                            }
                            let opts = Options {
                                source: cfg.source.clone(),
                                output,
                                allow_unverified: cfg.allow_unverified,
                                offline: cfg.offline,
                            };
                            let worker_info = info.clone();
                            std::thread::spawn(move || {
                                let tx2 = tx.clone();
                                let mut log = |l| {
                                    let _ = tx2.send(Msg::Log(l));
                                };
                                let r = if recomp {
                                    build::recompile(
                                        &worker_info,
                                        &build::RecompileOptions {
                                            source: opts.source,
                                            output: opts.output,
                                            allow_unverified: opts.allow_unverified,
                                            offline: opts.offline,
                                            allow_incomplete: false,
                                            keep_source: true,
                                            with_acceptance: false,
                                            jobs: None,
                                            with_interpreter: false,
                                            cache_dir: Some(build::default_cache_dir()),
                                        },
                                        &mut log,
                                    )
                                } else {
                                    build::build(&worker_info, &opts, &mut log)
                                };
                                let _ = tx.send(Msg::Done(r.map_err(|e| e.to_string())));
                            });
                            screen = Screen::Building {
                                info,
                                log: Vec::new(),
                                rx,
                                result: None,
                                play,
                            };
                            continue;
                        }
                    }
                    _ => {}
                }
                Screen::Menu { info, list }
            }
            Screen::Building {
                info,
                log,
                rx,
                result,
                play,
            } => {
                if result.is_some()
                    && matches!(key.code, KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q'))
                {
                    if let (Some(Ok(bundle)), true) = (&result, play) {
                        launch = Some(bundle.clone());
                    }
                    if launch.is_some() || key.code != KeyCode::Enter {
                        return Ok(launch);
                    }
                    Screen::Menu {
                        info,
                        list: ListState::default().with_selected(Some(0)),
                    }
                } else {
                    Screen::Building {
                        info,
                        log,
                        rx,
                        result,
                        play,
                    }
                }
            }
        };
    }
}

fn draw(f: &mut Frame, screen: &Screen, cfg: &Config) {
    let [title, body, help] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(1),
    ])
    .areas(f.area());
    f.render_widget(
        Paragraph::new("MKDS native builder")
            .block(Block::bordered())
            .style(Style::new().add_modifier(Modifier::BOLD)),
        title,
    );
    let (content, hint): (Vec<Line>, &str) = match screen {
        Screen::Path { input, error } => {
            let mut v = vec![
                Line::from(
                    "Enter the path to your own legally obtained MKDS .nds file (US, rev 0):",
                ),
                Line::from(format!("> {input}_")),
            ];
            if let Some(e) = error {
                v.push(Line::styled(
                    format!("Error: {e}"),
                    Style::new().fg(Color::Red),
                ));
            }
            (v, "Enter: validate   Esc: quit")
        }
        Screen::Menu { info, list } => {
            let [top, bottom] =
                Layout::vertical([Constraint::Length(9), Constraint::Min(3)]).areas(body);
            f.render_widget(
                Paragraph::new(rom_lines(info))
                    .block(Block::new().borders(Borders::ALL).title("ROM")),
                top,
            );
            let items: Vec<ListItem> = menu_items(info, cfg.allow_unverified)
                .into_iter()
                .map(ListItem::new)
                .collect();
            let list_w = List::new(items)
                .block(Block::bordered().title("Action"))
                .highlight_symbol("> ")
                .highlight_style(Style::new().fg(Color::Yellow));
            f.render_stateful_widget(list_w, bottom, &mut list.clone());
            f.render_widget(
                Paragraph::new("Up/Down: select   Enter: run   Esc: quit"),
                help,
            );
            return;
        }
        Screen::Building { log, result, .. } => {
            let rows = body.height.saturating_sub(2) as usize;
            let mut v: Vec<Line> = log
                .iter()
                .rev()
                .take(rows.saturating_sub(2))
                .rev()
                .map(|l| Line::from(l.as_str()))
                .collect();
            match result {
                None => v.push(Line::styled("Building...", Style::new().fg(Color::Cyan))),
                Some(Ok(p)) => v.push(Line::styled(
                    format!("Done: {}", p.display()),
                    Style::new().fg(Color::Green),
                )),
                Some(Err(e)) => v.push(Line::styled(
                    format!("Failed: {e}"),
                    Style::new().fg(Color::Red),
                )),
            }
            (
                v,
                if result.is_some() {
                    "Enter: continue (launches if chosen)   Esc: quit"
                } else {
                    "Please wait"
                },
            )
        }
    };
    f.render_widget(
        Paragraph::new(content)
            .wrap(Wrap { trim: false })
            .block(Block::bordered()),
        body,
    );
    f.render_widget(Paragraph::new(hint), help);
}

fn rom_lines(info: &RomInfo) -> Vec<Line<'static>> {
    let status = if info.reference_match {
        Color::Green
    } else {
        Color::Yellow
    };
    vec![
        Line::from(info.path.clone()),
        Line::from(format!(
            "Game {} revision {}   {} files, {} course archives",
            info.game_code,
            info.revision,
            info.file_count,
            info.courses.len()
        )),
        Line::styled(
            format!("Compatibility: {}", info.compatibility),
            Style::new().fg(status),
        ),
        Line::from(format!("SHA-256: {}", info.sha256)),
    ]
}
