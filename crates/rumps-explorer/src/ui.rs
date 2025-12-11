//! UI rendering for the RUMPS database explorer.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table};
use ratatui::Frame;
use rumps_types::DataStatus;

use crate::app::{App, Screen};

/// Column width for shortcut display (e.g., `[abc]`).
const SHORTCUT_WIDTH: u16 = 7;

impl App {
    /// Renders the entire UI: header, main content, footer, and optional legend overlay.
    pub(crate) fn render(&mut self, f: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Min(1),    // main content
            Constraint::Length(3), // footer
        ])
        .split(f.area());

        // Calculate items per page from available height (minus table header + margin)
        self.items_per_page = chunks[1].height.saturating_sub(2) as usize;

        self.render_header(f, chunks[0]);
        self.render_main(f, chunks[1]);
        self.render_footer(f, chunks[2]);

        if self.show_legend {
            self.render_legend(f);
        }
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
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" | ", Style::default().fg(Color::DarkGray)),
            Span::styled(title, Style::default().fg(Color::LightBlue)),
            Span::styled(" | ", Style::default().fg(Color::DarkGray)),
            Span::styled(&self.db_path, Style::default().fg(Color::DarkGray)),
        ]))
        .block(Block::default().borders(Borders::BOTTOM));

        f.render_widget(header, area);
    }

    fn render_main(&self, f: &mut Frame, area: Rect) {
        match self.is_loading() {
            true => {
                let loading = Paragraph::new(Span::styled(
                    "Loading...",
                    Style::default().fg(Color::Yellow),
                ));
                f.render_widget(loading, area);
            }
            false => self.render_items(f, area),
        }
    }

    fn render_items(&self, f: &mut Frame, area: Rect) {
        let rows = self.visible_items().iter().map(|item| {
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
            Row::new(vec!["", "Name", "Flags", "Value"])
                .style(
                    Style::default()
                        .fg(Color::LightBlue)
                        .add_modifier(Modifier::BOLD),
                )
                .bottom_margin(1),
        );

        f.render_widget(table, area);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let total = self.total_pages();
        let has_pages = total > 1;

        let base_hints = match &self.screen {
            Screen::Globals => vec![
                Span::styled("[a-z]", Style::default().fg(Color::Yellow)),
                Span::raw(" descend "),
                Span::styled("[q]", Style::default().fg(Color::Yellow)),
                Span::raw(" quit "),
                Span::styled("[?]", Style::default().fg(Color::Yellow)),
                Span::raw(" help"),
            ],
            Screen::Subscripts { .. } => vec![
                Span::styled("[a-z]", Style::default().fg(Color::Yellow)),
                Span::raw(" descend "),
                Span::styled("[u]", Style::default().fg(Color::Yellow)),
                Span::raw(" up "),
                Span::styled("[g]", Style::default().fg(Color::Yellow)),
                Span::raw(" globals "),
                Span::styled("[q]", Style::default().fg(Color::Yellow)),
                Span::raw(" quit "),
                Span::styled("[?]", Style::default().fg(Color::Yellow)),
                Span::raw(" help"),
            ],
        };

        let page_hints: Vec<Span> = match has_pages {
            true => vec![
                Span::raw(" "),
                Span::styled("[<-/->]", Style::default().fg(Color::Yellow)),
                Span::raw(" page "),
            ],
            false => vec![],
        };

        let input_display = match self.input.is_empty() {
            true => vec![],
            false => vec![
                Span::raw(" > "),
                Span::styled(&self.input, Style::default().fg(Color::LightRed)),
            ],
        };

        let page_indicator: Vec<Span> = match has_pages {
            true => vec![
                Span::raw(" "),
                Span::styled(
                    format!("{}/{}", self.page + 1, total),
                    Style::default().fg(Color::Cyan),
                ),
            ],
            false => vec![],
        };

        let footer = Paragraph::new(Line::from(
            base_hints
                .into_iter()
                .chain(page_hints)
                .chain(input_display)
                .chain(page_indicator)
                .collect::<Vec<_>>(),
        ))
        .block(Block::default().borders(Borders::TOP));

        f.render_widget(footer, area);
    }

    fn render_legend(&self, f: &mut Frame) {
        let area = centered_rect(40, 12, f.area());

        let legend_text = vec![
            Line::from(vec![Span::styled(
                "Legend",
                Style::default().add_modifier(Modifier::BOLD),
            )]),
            Line::from(""),
            Line::from(vec![Span::styled(
                "Flags:",
                Style::default().add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![
                Span::styled("  V", Style::default().fg(Color::Green)),
                Span::raw(" = node has value"),
            ]),
            Line::from(vec![
                Span::styled("  +", Style::default().fg(Color::Blue)),
                Span::raw(" = node has children"),
            ]),
            Line::from(vec![
                Span::styled("  .", Style::default().fg(Color::DarkGray)),
                Span::raw(" = absent"),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Press ", Style::default().fg(Color::DarkGray)),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::styled(" or ", Style::default().fg(Color::DarkGray)),
                Span::styled("?", Style::default().fg(Color::Yellow)),
                Span::styled(" to close", Style::default().fg(Color::DarkGray)),
            ]),
        ];

        let legend = Paragraph::new(legend_text).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::LightBlue))
                .title(" Help "),
        );

        f.render_widget(Clear, area);
        f.render_widget(legend, area);
    }
}

/// Formats `DataStatus` as a 2-char flag string (V=value, +=children).
fn format_flags(status: DataStatus) -> String {
    match status {
        DataStatus::NoData => "..".to_string(),
        DataStatus::HasValue => "V.".to_string(),
        DataStatus::HasDescendants => ".+".to_string(),
        DataStatus::Both => "V+".to_string(),
    }
}

/// Truncates string to `max` chars, adding `...` if needed.
fn truncate(s: &str, max: usize) -> String {
    (s.len() > max)
        .then(|| format!("{}...", &s[..max - 3]))
        .unwrap_or_else(|| s.to_string())
}

/// Returns a centered rectangle of the given size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
