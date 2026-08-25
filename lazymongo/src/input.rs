//! A minimal single-line text input, reused by modal fields and prompts.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

#[derive(Default, Clone)]
pub struct Input {
    pub text: String,
    pub cursor: usize, // char index
}

impl Input {
    pub fn with_text(text: impl Into<String>) -> Self {
        let text = text.into();
        let cursor = text.chars().count();
        Self { text, cursor }
    }

    /// Returns true if the key was consumed.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        // Ctrl-modified chars are commands (Ctrl-S, Ctrl-L, ...), not text.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        match key.code {
            KeyCode::Backspace => {
                if self.cursor > 0 {
                    let idx = char_to_byte(&self.text, self.cursor - 1);
                    self.text.remove(idx);
                    self.cursor -= 1;
                }
                true
            }
            KeyCode::Delete => {
                if self.cursor < self.text.chars().count() {
                    let idx = char_to_byte(&self.text, self.cursor);
                    self.text.remove(idx);
                }
                true
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.text.chars().count());
                true
            }
            KeyCode::Home => {
                self.cursor = 0;
                true
            }
            KeyCode::End => {
                self.cursor = self.text.chars().count();
                true
            }
            KeyCode::Char(c) => {
                let idx = char_to_byte(&self.text, self.cursor);
                self.text.insert(idx, c);
                self.cursor += 1;
                true
            }
            _ => false,
        }
    }

    /// Spans for rendering within `width` cells: the view scrolls
    /// horizontally to keep the cursor visible, with `…` markers when
    /// content continues beyond either edge.
    pub fn spans(&self, focused: bool, width: u16) -> Vec<Span<'static>> {
        input_spans(&self.text, self.cursor, focused, width)
    }
}

/// Shared single-line input renderer (also used by the query bar).
pub fn input_spans(text: &str, cursor: usize, focused: bool, width: u16) -> Vec<Span<'static>> {
    let width = width.max(4) as usize;
    let chars: Vec<char> = text.chars().collect();
    if !focused {
        // Unfocused: show the head, ellipsized.
        if chars.len() > width {
            let head: String = chars[..width - 1].iter().collect();
            return vec![Span::raw(head + "…")];
        }
        return vec![Span::raw(text.to_string())];
    }
    // Window [start, start+avail) chosen so the cursor stays visible.
    let avail = width - 1; // one cell reserved for the cursor block
    let start = cursor.saturating_sub(avail.saturating_sub(1));
    let scrolled_left = start > 0;
    let visible: Vec<char> = chars.iter().skip(start).take(avail).copied().collect();
    let cur = cursor - start;
    let mut before: String = visible.iter().take(cur).collect();
    if scrolled_left && !before.is_empty() {
        before.replace_range(
            ..before.chars().next().map(char::len_utf8).unwrap_or(0),
            "…",
        );
    }
    let at: String = visible
        .get(cur)
        .map(|c| c.to_string())
        .unwrap_or_else(|| " ".into());
    let mut after: String = visible.iter().skip(cur + 1).collect();
    if start + avail < chars.len() && !after.is_empty() {
        let cut = after.len() - after.chars().last().map(char::len_utf8).unwrap_or(0);
        after.truncate(cut);
        after.push('…');
    }
    vec![
        Span::raw(before),
        Span::styled(at, Style::new().add_modifier(Modifier::REVERSED)),
        Span::raw(after),
    ]
}

pub fn char_to_byte(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(i, _)| i)
        .unwrap_or(s.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(spans: &[Span]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn long_input_follows_cursor() {
        let text: String = ('a'..='z').cycle().take(100).collect();
        // cursor at the end, narrow width -> tail visible with left marker
        let spans = input_spans(&text, 100, true, 20);
        let out = flat(&spans);
        assert!(out.starts_with('…'), "{out}");
        assert!(out.len() <= 21);
        assert!(out.trim_end().ends_with(text.chars().last().unwrap()) || out.ends_with(' '));
        // cursor at the start -> head visible with right marker
        let spans = input_spans(&text, 0, true, 20);
        let out = flat(&spans);
        assert!(out.starts_with('a'), "{out}");
        assert!(out.ends_with('…'), "{out}");
    }

    #[test]
    fn short_input_untouched() {
        let spans = input_spans("abc", 3, true, 40);
        assert_eq!(flat(&spans), "abc ");
        let spans = input_spans("abc", 0, false, 40);
        assert_eq!(flat(&spans), "abc");
    }
}
