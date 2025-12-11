use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{App, Screen};

const SHORTCUT_WIDTH: u16 = 7; // "[aaa] " + padding

impl App {
    pub fn render(&self, f: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Min(1),    // main
            Constraint::Length(3), // footer
        ])
        .split(f.area());

        self.render_header(f, chunks[0]);
        self.render_main(f, chunks[1]);
        self.render_footer(f, chunks[2]);
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let title = match &self.screen {
            Screen::Globals => "Globals".to_string(),
            Screen::Subscripts { global, path } => {
                let subs = path
                    .iter()
                    .map(|s| format!("{}", s))
                    .collect::<Vec<_>>()
                    .join(",");
                match subs.is_empty() {
                    true => format!("^{}", global),
                    false => format!("^{}({})", global, subs),
                }
            }
        };

        let header = Paragraph::new(Line::from(vec![
            Span::styled(
                "RUMPS Explorer",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" | "),
            Span::styled(title, Style::default().fg(Color::Cyan)),
            Span::raw(" | "),
            Span::raw(&self.db_path),
        ]))
        .block(Block::default().borders(Borders::BOTTOM));

        f.render_widget(header, area);
    }

    fn render_main(&self, f: &mut Frame, area: Rect) {
        match self.is_loading() {
            true => {
                let loading = Paragraph::new("Loading...");
                f.render_widget(loading, area);
            }
            false => self.render_items(f, area),
        }
    }

    fn render_items(&self, f: &mut Frame, area: Rect) {
        let rows = self.items.iter().map(|item| {
            let shortcut = format!("[{}]", item.shortcut);
            let name = format!("{}", item.subscript);
            let flags = format_flags(item.flags);
            let preview = item
                .preview
                .as_ref()
                .map(|p| truncate(p, 40))
                .unwrap_or_default();

            Row::new(vec![shortcut, name, flags, preview])
                .style(Style::default())
        });

        let table = Table::new(
            rows,
            [
                Constraint::Length(SHORTCUT_WIDTH),
                Constraint::Min(10),
                Constraint::Length(4),
                Constraint::Min(20),
            ],
        )
        .header(
            Row::new(vec!["Key", "Name", "Flags", "Value"])
                .style(Style::default().add_modifier(Modifier::BOLD))
                .bottom_margin(1),
        );

        f.render_widget(table, area);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let hints = match &self.screen {
            Screen::Globals => "[a-z] descend | [q] quit | [?] help",
            Screen::Subscripts { .. } => {
                "[a-z] descend | [u] up | [g] globals | [q] quit | [?] help"
            }
        };

        let input_display = match self.input.is_empty() {
            true => String::new(),
            false => format!(" > {}", self.input),
        };

        let footer = Paragraph::new(Line::from(vec![
            Span::raw(hints),
            Span::styled(input_display, Style::default().fg(Color::Green)),
        ]))
        .block(Block::default().borders(Borders::TOP));

        f.render_widget(footer, area);
    }
}

fn format_flags(status: rumps_types::DataStatus) -> String {
    use rumps_types::DataStatus;
    match status {
        DataStatus::NoData => "..".to_string(),
        DataStatus::HasValue => "V.".to_string(),
        DataStatus::HasDescendants => ".+".to_string(),
        DataStatus::Both => "V+".to_string(),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() > max {
        format!("{}...", &s[..max - 3])
    } else {
        s.to_string()
    }
}
