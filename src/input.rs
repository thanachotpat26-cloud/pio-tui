//! Minimal single-line text input with readline-style keybindings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default, Clone)]
pub struct Input {
    chars: Vec<char>,
    cursor: usize,
}

impl Input {
    pub fn new(s: &str) -> Self {
        let chars: Vec<char> = s.chars().collect();
        let cursor = chars.len();
        Self { chars, cursor }
    }

    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Handle a key. Returns `true` when the text content changed.
    pub fn handle(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char('u') if ctrl => {
                let changed = self.cursor > 0;
                self.chars.drain(..self.cursor);
                self.cursor = 0;
                changed
            }
            KeyCode::Char('w') if ctrl => self.delete_word(),
            KeyCode::Backspace if ctrl || alt => self.delete_word(),
            KeyCode::Char('a') if ctrl => {
                self.cursor = 0;
                false
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = self.chars.len();
                false
            }
            KeyCode::Char(c) if !ctrl && !alt => {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
                true
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
                true
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
                true
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                false
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(self.chars.len());
                false
            }
            KeyCode::Home => {
                self.cursor = 0;
                false
            }
            KeyCode::End => {
                self.cursor = self.chars.len();
                false
            }
            _ => false,
        }
    }

    fn delete_word(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let mut start = self.cursor;
        while start > 0 && self.chars[start - 1] == ' ' {
            start -= 1;
        }
        while start > 0 && !matches!(self.chars[start - 1], ' ' | '/' | '-' | '_') {
            start -= 1;
        }
        if start == self.cursor {
            start -= 1;
        }
        self.chars.drain(start..self.cursor);
        self.cursor = start;
        true
    }

    /// Text visible in a box of `width` columns (scrolled so the cursor is
    /// visible), and the cursor column inside it.
    pub fn view(&self, width: usize) -> (String, usize) {
        let width = width.max(1);
        let off = if self.cursor >= width { self.cursor + 1 - width } else { 0 };
        let s: String = self.chars.iter().skip(off).take(width).collect();
        (s, self.cursor - off)
    }
}
