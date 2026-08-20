//! A minimal multi-line text editor widget for JSON documents and pipelines.
//! No wrapping: long lines clip horizontally and follow the cursor.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};

use crate::theme;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::input::char_to_byte;

pub struct TextArea {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize, // char index within the line
    pub scroll_row: usize,
    pub scroll_col: usize,
}

impl TextArea {
    pub fn from_text(text: &str) -> Self {
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.lines().map(str::to_string).collect()
        };
        Self {
            lines,
            row: 0,
            col: 0,
            scroll_row: 0,
            scroll_col: 0,
        }
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    fn line_len(&self, row: usize) -> usize {
        self.lines.get(row).map(|l| l.chars().count()).unwrap_or(0)
    }

    /// Handle a key. Returns true if consumed. Enter inserts a newline;
    /// submission (Ctrl-S) is handled by the caller.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match key.code {
            KeyCode::Char(c) => {
                let line = &mut self.lines[self.row];
                let idx = char_to_byte(line, self.col);
                line.insert(idx, c);
                self.col += 1;
            }
            KeyCode::Enter => {
                let line = &mut self.lines[self.row];
                let idx = char_to_byte(line, self.col);
                let rest = line.split_off(idx);
                // Auto-indent: carry over leading spaces of the current line.
                let indent: String = self.lines[self.row]
                    .chars()
                    .take_while(|c| *c == ' ')
                    .collect();
                self.lines.insert(self.row + 1, format!("{indent}{rest}"));
                self.row += 1;
                self.col = indent.chars().count();
            }
            KeyCode::Backspace => {
                if self.col > 0 {
                    let line = &mut self.lines[self.row];
                    let idx = char_to_byte(line, self.col - 1);
                    line.remove(idx);
                    self.col -= 1;
                } else if self.row > 0 {
                    let removed = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                    self.lines[self.row].push_str(&removed);
                }
            }
            KeyCode::Delete => {
                let len = self.line_len(self.row);
                if self.col < len {
                    let line = &mut self.lines[self.row];
                    let idx = char_to_byte(line, self.col);
                    line.remove(idx);
                } else if self.row + 1 < self.lines.len() {
                    let removed = self.lines.remove(self.row + 1);
                    self.lines[self.row].push_str(&removed);
                }
            }
            KeyCode::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.line_len(self.row);
                }
            }
            KeyCode::Right => {
                if self.col < self.line_len(self.row) {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            KeyCode::Up => {
                self.row = self.row.saturating_sub(1);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::Down => {
                self.row = (self.row + 1).min(self.lines.len() - 1);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::Home => self.col = 0,
            KeyCode::End => self.col = self.line_len(self.row),
            KeyCode::PageUp => {
                self.row = self.row.saturating_sub(10);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::PageDown => {
                self.row = (self.row + 10).min(self.lines.len() - 1);
                self.col = self.col.min(self.line_len(self.row));
            }
            KeyCode::Tab => {
                let line = &mut self.lines[self.row];
                let idx = char_to_byte(line, self.col);
                line.insert_str(idx, "  ");
                self.col += 2;
            }
            _ => return false,
        }
        true
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, focused: bool) {
        let height = area.height as usize;
        let width = area.width as usize;

        // Follow the cursor.
        if self.row < self.scroll_row {
            self.scroll_row = self.row;
        } else if height > 0 && self.row >= self.scroll_row + height {
            self.scroll_row = self.row - height + 1;
        }
        if self.col < self.scroll_col {
            self.scroll_col = self.col;
        } else if width > 4 && self.col >= self.scroll_col + width - 4 {
            self.scroll_col = self.col - (width - 4) + 1;
        }

        let num_w = self.lines.len().to_string().len().max(2);
        let mut out: Vec<Line> = Vec::with_capacity(height);
        for (i, line) in self
            .lines
            .iter()
            .enumerate()
            .skip(self.scroll_row)
            .take(height)
        {
            let mut spans = vec![Span::styled(
                format!("{:>num_w$} ", i + 1),
                Style::new().fg(theme::dim()),
            )];
            let chars: Vec<char> = line.chars().collect();
            let visible: Vec<char> = chars
                .iter()
                .skip(self.scroll_col)
                .take(width.saturating_sub(num_w + 1))
                .copied()
                .collect();
            if focused && i == self.row {
                let cur = self.col - self.scroll_col.min(self.col);
                let before: String = visible.iter().take(cur).collect();
                let at: String = visible
                    .get(cur)
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| " ".into());
                let after: String = visible.iter().skip(cur + 1).collect();
                spans.push(Span::raw(before));
                spans.push(Span::styled(
                    at,
                    Style::new().add_modifier(Modifier::REVERSED),
                ));
                spans.push(Span::raw(after));
            } else {
                spans.push(Span::raw(visible.iter().collect::<String>()));
            }
            out.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(Text::from(out)), area);
    }
}
