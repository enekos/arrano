mod app;
mod claude;
mod gh;
mod linear;
mod llm;
mod md;
mod model;
mod repo;
mod ui;

use std::io::IsTerminal;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
use ratatui::crossterm::execute;

use crate::app::App;

const USAGE: &str = "arrano — eagle view over your GitHub PRs

usage: arrano [--org <org>]

  -o, --org <org>   only show PRs in one org/owner (e.g. --org my-org)
  -h, --help        show this help";

fn parse_args() -> Option<String> {
    let mut org: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" | "--org" => match args.next() {
                Some(v) => org = Some(v),
                None => {
                    eprintln!("--org needs a value\n\n{USAGE}");
                    std::process::exit(2);
                }
            },
            _ if a.starts_with("--org=") => org = Some(a["--org=".len()..].to_string()),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument: {other}\n\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    org
}

fn main() -> Result<()> {
    let org = parse_args();
    if !std::io::stdout().is_terminal() {
        eprintln!("arrano is a TUI — run it in a terminal");
        std::process::exit(1);
    }

    let (tx, rx) = mpsc::channel();
    let mut app = App::new(tx, org);
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);

    let result = (|| -> Result<()> {
        loop {
            while let Ok(ev) = rx.try_recv() {
                app.on_app_event(ev);
            }
            app.poll_debounce();
            app.poll_auto_refresh();
            app.expire_toasts();
            app.tick_anim();
            terminal.draw(|f| ui::draw(f, &mut app))?;
            // animate at ~60fps while a smooth scroll is in flight
            let timeout = if app.anim.is_some() {
                Duration::from_millis(15)
            } else {
                Duration::from_millis(80)
            };
            if event::poll(timeout)? {
                match event::read()? {
                    Event::Key(k) if k.kind == KeyEventKind::Press => app.on_key(k),
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollDown => {
                            app.on_mouse_scroll(true, m.column, m.row)
                        }
                        MouseEventKind::ScrollUp => {
                            app.on_mouse_scroll(false, m.column, m.row)
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            app.tick += 1;
            if app.quit {
                return Ok(());
            }
        }
    })();

    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
