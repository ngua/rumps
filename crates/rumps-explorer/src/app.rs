use std::io;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers,
};
use futures::future::BoxFuture;
use futures::StreamExt;
use miette::{IntoDiagnostic, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use rumps_storage::Database;
use rumps_types::{DataStatus, Key, Subscript};

#[derive(Clone)]
pub enum Screen {
    Globals,
    Subscripts { global: String, path: Key },
}

pub enum Action {
    None,
    Quit,
    Navigate(Screen),
}

#[derive(Clone)]
pub struct Item {
    pub subscript: Subscript,
    pub shortcut: String,
    pub flags: DataStatus,
    pub preview: Option<String>,
}

pub struct App {
    pub db: Database,
    pub db_path: String,
    pub screen: Screen,
    pub items: Vec<Item>,
    pub input: String,
    pub pending: Option<BoxFuture<'static, Result<Vec<Item>>>>,
}

impl App {
    pub fn new(db: Database, db_path: String) -> Self {
        Self {
            db,
            db_path,
            screen: Screen::Globals,
            items: Vec::new(),
            input: String::new(),
            pending: None,
        }
    }

    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn handle_input(&mut self, ev: Event) -> Action {
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match key.code {
                    KeyCode::Char('q') => Action::Quit,
                    KeyCode::Char('c')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        Action::Quit
                    }
                    KeyCode::Char('d')
                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                    {
                        Action::Quit
                    }
                    _ => Action::None,
                }
            }
            _ => Action::None,
        }
    }
}

pub async fn run(
    db: Database,
    db_path: String,
    mut term: Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let mut app = App::new(db, db_path);
    let mut events = EventStream::new();

    // Initial load of globals
    let db_clone = app.db.clone();
    app.pending = Some(Box::pin(async move {
        let names = db_clone.list_globals().await;
        let width = shortcut_width(names.len());
        Ok(names
            .into_iter()
            .enumerate()
            .map(|(i, name)| Item {
                subscript: Subscript::String(name),
                shortcut: shortcut(i, width),
                flags: DataStatus::HasDescendants,
                preview: None,
            })
            .collect())
    }));

    loop {
        term.draw(|f| app.render(f)).into_diagnostic()?;

        tokio::select! {
            Some(Ok(ev)) = events.next() => {
                match app.handle_input(ev) {
                    Action::Quit => break,
                    Action::Navigate(_screen) => {
                        // TODO: set up pending load for new screen
                    }
                    Action::None => {}
                }
            }
            res = async { app.pending.as_mut().unwrap().await }, if app.pending.is_some() => {
                app.items = res?;
                app.pending = None;
            }
        }
    }

    Ok(())
}

fn shortcut_width(count: usize) -> usize {
    match count {
        0..=26 => 1,
        27..=676 => 2,
        _ => 3,
    }
}

fn shortcut(idx: usize, width: usize) -> String {
    (0..width).fold(String::with_capacity(width), |mut acc, pos| {
        let divisor = 26usize.pow((width - 1 - pos) as u32);
        let c = ((idx / divisor) % 26) as u8 + b'a';
        acc.push(c as char);
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_width_single() {
        assert_eq!(shortcut_width(1), 1);
        assert_eq!(shortcut_width(26), 1);
    }

    #[test]
    fn shortcut_width_double() {
        assert_eq!(shortcut_width(27), 2);
        assert_eq!(shortcut_width(676), 2);
    }

    #[test]
    fn shortcut_width_triple() {
        assert_eq!(shortcut_width(677), 3);
    }

    #[test]
    fn shortcut_single() {
        assert_eq!(shortcut(0, 1), "a");
        assert_eq!(shortcut(25, 1), "z");
    }

    #[test]
    fn shortcut_double() {
        assert_eq!(shortcut(0, 2), "aa");
        assert_eq!(shortcut(26, 2), "ba");
        assert_eq!(shortcut(675, 2), "zz");
    }

    #[test]
    fn shortcut_triple() {
        assert_eq!(shortcut(0, 3), "aaa");
        assert_eq!(shortcut(26, 3), "aba");
    }
}
