use std::io;
use std::num::NonZeroUsize;

use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers,
};
use futures::future::BoxFuture;
use futures::StreamExt;
use lru::LruCache;
use miette::{IntoDiagnostic, Result};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use rumps_storage::Database;
use rumps_types::{DataStatus, Key, Subscript};

const CACHE_SIZE: usize = 64;

pub async fn run(
    db: Database,
    db_path: String,
    mut term: Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let mut app = App::new(db, db_path);
    let mut events = EventStream::new();

    // Initial load of globals
    app.navigate_to(Screen::Globals);

    loop {
        term.draw(|f| app.render(f)).into_diagnostic()?;

        tokio::select! {
            Some(Ok(ev)) = events.next() => {
                match app.handle_input(ev) {
                    Action::Quit => break,
                    Action::Navigate(screen) => app.navigate_to(screen),
                    Action::None => {}
                }
            }
            res = async { app.pending.as_mut().unwrap().await }, if app.pending.is_some() => {
                app.items = res?;
                app.pending = None;
                app.cache_items();
            }
        }
    }

    Ok(())
}

#[derive(Clone)]
pub enum Screen {
    Globals,
    Subscripts { global: String, path: Key },
}

impl Screen {
    fn cache_key(&self) -> String {
        match self {
            Screen::Globals => "globals".to_string(),
            Screen::Subscripts { global, path } => {
                let subs = path
                    .iter()
                    .map(|s| format!("{}", s))
                    .collect::<Vec<_>>()
                    .join(",");
                format!("^{}({})", global, subs)
            }
        }
    }
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
    cache: LruCache<String, Vec<Item>>,
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
            // SAFETY: CACHE_SIZE is a non-zero constant
            #[allow(clippy::unwrap_used)]
            cache: LruCache::new(NonZeroUsize::new(CACHE_SIZE).unwrap()),
        }
    }

    pub fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    pub fn handle_input(&mut self, ev: Event) -> Action {
        match ev {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match key.code {
                    // Quit commands
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

                    // Clear input
                    KeyCode::Esc | KeyCode::Backspace => {
                        self.input.clear();
                        Action::None
                    }

                    // Navigation: up one level
                    KeyCode::Char('u') if self.input.is_empty() => {
                        match &self.screen {
                            Screen::Globals => Action::None,
                            Screen::Subscripts { global, path } => {
                                match path.len() {
                                    0 | 1 => Action::Navigate(Screen::Globals),
                                    _ => {
                                        let mut new_path = path.clone();
                                        new_path.pop();
                                        Action::Navigate(Screen::Subscripts {
                                            global: global.clone(),
                                            path: new_path,
                                        })
                                    }
                                }
                            }
                        }
                    }

                    // Navigation: back to globals
                    KeyCode::Char('g') if self.input.is_empty() => {
                        match &self.screen {
                            Screen::Globals => Action::None,
                            Screen::Subscripts { .. } => {
                                Action::Navigate(Screen::Globals)
                            }
                        }
                    }

                    // Quit (only when input is empty)
                    KeyCode::Char('q') if self.input.is_empty() => Action::Quit,

                    // Letter input for shortcuts
                    KeyCode::Char(c) if c.is_ascii_lowercase() => {
                        self.input.push(c);
                        self.try_navigate()
                    }

                    // Enter to confirm shortcut
                    KeyCode::Enter => {
                        let action = self.try_navigate_exact();
                        self.input.clear();
                        action
                    }

                    _ => Action::None,
                }
            }
            _ => Action::None,
        }
    }

    fn try_navigate(&mut self) -> Action {
        let matches: Vec<_> = self
            .items
            .iter()
            .filter(|item| item.shortcut.starts_with(&self.input))
            .collect();

        match matches.len() {
            0 => {
                self.input.clear();
                Action::None
            }
            1 if matches[0].shortcut == self.input => {
                let item = matches[0];
                let action = self.navigate_to_item(item);
                self.input.clear();
                action
            }
            _ => Action::None, // multiple matches, wait for more input
        }
    }

    fn try_navigate_exact(&self) -> Action {
        self.items
            .iter()
            .find(|item| item.shortcut == self.input)
            .map(|item| self.navigate_to_item(item))
            .unwrap_or(Action::None)
    }

    fn navigate_to_item(&self, item: &Item) -> Action {
        match &self.screen {
            Screen::Globals => {
                let global = match &item.subscript {
                    Subscript::String(s) => s.clone(),
                    _ => format!("{}", item.subscript),
                };
                Action::Navigate(Screen::Subscripts {
                    global,
                    path: Key::new(),
                })
            }
            Screen::Subscripts { global, path } => {
                let mut new_path = path.clone();
                new_path.push(item.subscript.clone());
                Action::Navigate(Screen::Subscripts {
                    global: global.clone(),
                    path: new_path,
                })
            }
        }
    }

    pub fn navigate_to(&mut self, screen: Screen) {
        let key = screen.cache_key();
        self.screen = screen.clone();
        self.input.clear();

        match self.cache.get(&key).cloned() {
            Some(items) => {
                self.items = items;
                self.pending = None;
            }
            None => {
                self.items.clear();
                self.pending = Some(self.load_screen(screen));
            }
        }
    }

    pub fn cache_items(&mut self) {
        let key = self.screen.cache_key();
        self.cache.put(key, self.items.clone());
    }

    fn load_screen(
        &self,
        screen: Screen,
    ) -> BoxFuture<'static, Result<Vec<Item>>> {
        let db = self.db.clone();
        match screen {
            Screen::Globals => {
                Box::pin(async move { Self::load_globals(db).await })
            }
            Screen::Subscripts { global, path } => Box::pin(async move {
                Self::load_subscripts(db, &global, &path).await
            }),
        }
    }

    async fn load_globals(db: Database) -> Result<Vec<Item>> {
        let names = db.list_globals().await;
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
    }

    async fn load_subscripts(
        _db: Database,
        _global: &str,
        _path: &Key,
    ) -> Result<Vec<Item>> {
        // TODO: implement actual subscript loading
        Ok(Vec::new())
    }
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
