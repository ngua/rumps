//! Application state and event loop for the RUMPS database explorer.

use std::collections::BTreeSet;
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
use rumps_types::{global, DataStatus, Key, Subscript};

/// Max entries in the navigation cache.
const CACHE_SIZE: usize = 64;

/// Main event loop. Renders UI and handles input/async data loading via `select!`.
// NOTE: The `unwrap()` in `select!` is guarded by `if app.pending.is_some()`, but
// clippy can't see through the macro expansion to verify this.
#[allow(clippy::unwrap_used)]
pub async fn run(
    db: Database,
    db_path: String,
    mut term: Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let mut app = App::new(db, db_path);
    let mut events = EventStream::new();

    app.navigate_to(Screen::Globals);

    loop {
        // NOTE: `render()` updates `app.items_per_page` based on terminal size
        term.draw(|f| app.render(f)).into_diagnostic()?;

        tokio::select! {
            // Handle keyboard events
            Some(Ok(ev)) = events.next() => {
                match app.handle_input(ev) {
                    Action::Quit => break,
                    Action::Navigate(screen) => app.navigate_to(screen),
                    Action::None => {}
                }
            }
            // Handle async data load completion
            res = async { app.pending.as_mut().unwrap().await }, if app.pending.is_some() => {
                app.items = res?;
                app.pending = None;
                app.cache_items();
            }
        }
    }

    Ok(())
}

/// Current view in the explorer.
#[derive(Clone)]
pub(crate) enum Screen {
    /// Top-level list of all globals in the database.
    Globals,
    /// Subscript level within a specific global.
    Subscripts { global: String, path: Key },
}

impl Screen {
    /// Returns a unique string for caching this screen's items.
    fn cache_key(&self) -> String {
        match self {
            Self::Globals => "globals".to_string(),
            Self::Subscripts { global, path } => {
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

/// Result of processing a keyboard event.
enum Action {
    None,
    Quit,
    Navigate(Screen),
}

/// A single row in the item list (global name or subscript).
#[derive(Clone)]
pub(crate) struct Item {
    pub(crate) subscript: Subscript,
    pub(crate) shortcut: String,
    pub(crate) flags: DataStatus,
    pub(crate) preview: Option<String>,
}

/// Main application state.
pub(crate) struct App {
    db: Database,
    pub(crate) db_path: String,
    pub(crate) screen: Screen,
    /// All items at the current screen (may span multiple pages).
    items: Vec<Item>,
    /// Accumulated shortcut input (e.g., "ab" for shortcut `[ab]`).
    pub(crate) input: String,
    /// In-flight async load; `Some` while loading, `None` when idle.
    pending: Option<BoxFuture<'static, Result<Vec<Item>>>>,
    pub(crate) show_legend: bool,
    /// Current page index (0-based).
    pub(crate) page: usize,
    /// Items per page; updated by `render()` based on terminal height.
    pub(crate) items_per_page: usize,
    /// LRU cache of previously loaded screens.
    cache: LruCache<String, Vec<Item>>,
}

impl App {
    fn new(db: Database, db_path: String) -> Self {
        Self {
            db,
            db_path,
            screen: Screen::Globals,
            items: Vec::new(),
            input: String::new(),
            pending: None,
            show_legend: false,
            page: 0,
            items_per_page: 20, // default; updated at render time
            // SAFETY: CACHE_SIZE is a non-zero constant
            #[allow(clippy::unwrap_used)]
            cache: LruCache::new(NonZeroUsize::new(CACHE_SIZE).unwrap()),
        }
    }

    pub(crate) fn total_pages(&self) -> usize {
        match self.items_per_page {
            0 => 1,
            per_page => {
                let total = self.items.len();
                (total + per_page - 1) / per_page.max(1)
            }
        }
        .max(1)
    }

    /// Returns the slice of items visible on the current page.
    pub(crate) fn visible_items(&self) -> &[Item] {
        let start = self.page * self.items_per_page;
        let end = (start + self.items_per_page).min(self.items.len());
        self.items.get(start..end).unwrap_or(&[])
    }

    pub(crate) fn is_loading(&self) -> bool {
        self.pending.is_some()
    }

    /// Processes a terminal event and returns the resulting action.
    fn handle_input(&mut self, ev: Event) -> Action {
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

                    // Clear input or close legend
                    KeyCode::Esc => {
                        self.show_legend = false;
                        self.input.clear();
                        Action::None
                    }
                    KeyCode::Backspace => {
                        self.input.clear();
                        Action::None
                    }

                    // Toggle legend
                    KeyCode::Char('?') => {
                        self.show_legend = !self.show_legend;
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

                    // Pagination
                    KeyCode::Left => {
                        self.page = self.page.saturating_sub(1);
                        Action::None
                    }
                    KeyCode::Right => {
                        let max = self.total_pages().saturating_sub(1);
                        self.page = (self.page + 1).min(max);
                        Action::None
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

    /// Navigates to a new screen, loading from cache or initiating async fetch.
    fn navigate_to(&mut self, screen: Screen) {
        let key = screen.cache_key();
        self.screen = screen.clone();
        self.input.clear();
        self.page = 0;

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

    /// Stores current items in the LRU cache.
    fn cache_items(&mut self) {
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
        db: Database,
        glob: &str,
        path: &Key,
    ) -> Result<Vec<Item>> {
        let name = global!(glob);
        let depth = path.len();

        // Collect unique subscripts at the next level
        let results: Vec<Subscript> = db
            .collects_vec(
                &name,
                None,
                |k, _| k.len() > depth && k.starts_with(path),
                |k, _| k.get(depth).cloned(),
            )
            .await
            .into_diagnostic()?;

        let subs: BTreeSet<Subscript> = results.into_iter().collect();

        // Build items with flags and value preview
        // NOTE: This makes separate `db.data()` and `db.get()` calls per subscript.
        // Could be optimized to a single `collects_vec` pass that extracts flags and
        // values directly, but the current approach is simpler and good enough for now.
        let width = shortcut_width(subs.len());
        let items: Vec<Item> =
            futures::stream::iter(subs.into_iter().enumerate())
                .then(|(i, sub): (usize, Subscript)| {
                    let db = db.clone();
                    let name = name.clone();
                    let mut key = path.clone();
                    key.push(sub.clone());
                    async move {
                        let flags = db
                            .data(&name, &key)
                            .await
                            .unwrap_or(DataStatus::NoData);
                        let preview = match flags {
                            DataStatus::HasValue | DataStatus::Both => db
                                .get(&name, &key)
                                .await
                                .ok()
                                .flatten()
                                .map(|v| truncate_value(&v, 40)),
                            _ => None,
                        };
                        Item {
                            subscript: sub,
                            shortcut: shortcut(i, width),
                            flags,
                            preview,
                        }
                    }
                })
                .collect()
                .await;

        Ok(items)
    }
}

/// Truncates a value's display string to `max` chars, adding `...` if needed.
fn truncate_value(v: &rumps_types::Value, max: usize) -> String {
    let s = format!("{}", v);
    (s.len() > max)
        .then(|| format!("{}...", &s[..max - 3]))
        .unwrap_or(s)
}

/// Returns the number of letters needed for shortcuts given `count` items.
fn shortcut_width(count: usize) -> usize {
    match count {
        0..=26 => 1,   // a-z
        27..=676 => 2, // aa-zz
        _ => 3,        // aaa-zzz
    }
}

/// Generates a shortcut string (e.g., "a", "ab", "abc") for the given index.
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
