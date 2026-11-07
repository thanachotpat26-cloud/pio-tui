//! Single-line text input with readline-style keybindings.
//!
//! Unicode-aware: the cursor moves over grapheme clusters (so a Thai base
//! letter plus its vowel/tone marks is one step), and scrolling/cursor
//! placement use terminal display width (so CJK double-width text lines up).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[derive(Debug, Default, Clone)]
pub struct Input {
    value: String,
    /// Byte offset into `value`; always on a char boundary.
    cursor: usize,
}

impl Input {
    pub fn new(s: &str) -> Self {
        Self { value: s.to_owned(), cursor: s.len() }
    }

    pub fn value(&self) -> String {
        self.value.clone()
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn prev_boundary(&self) -> usize {
        self.value[..self.cursor].grapheme_indices(true).next_back().map_or(0, |(i, _)| i)
    }

    fn next_boundary(&self) -> usize {
        self.value[self.cursor..]
            .graphemes(true)
            .next()
            .map_or(self.cursor, |g| self.cursor + g.len())
    }

    /// Handle a key. Returns `true` when the text content changed.
    pub fn handle(&mut self, k: &KeyEvent) -> bool {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        match k.code {
            KeyCode::Char('u') if ctrl => {
                let changed = self.cursor > 0;
                self.value.drain(..self.cursor);
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
                self.cursor = self.value.len();
                false
            }
            KeyCode::Char(c) if !ctrl && !alt && !c.is_control() => {
                self.value.insert(self.cursor, c);
                self.cursor += c.len_utf8();
                true
            }
            // Backspace removes one code point, so a mistyped Thai tone mark
            // can be corrected without deleting its base consonant.
            KeyCode::Backspace => match self.value[..self.cursor].chars().next_back() {
                Some(c) => {
                    self.cursor -= c.len_utf8();
                    self.value.remove(self.cursor);
                    true
                }
                None => false,
            },
            KeyCode::Delete => {
                let end = self.next_boundary();
                let changed = end > self.cursor;
                self.value.drain(self.cursor..end);
                changed
            }
            KeyCode::Left => {
                self.cursor = self.prev_boundary();
                false
            }
            KeyCode::Right => {
                self.cursor = self.next_boundary();
                false
            }
            KeyCode::Home => {
                self.cursor = 0;
                false
            }
            KeyCode::End => {
                self.cursor = self.value.len();
                false
            }
            _ => false,
        }
    }

    fn delete_word(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let before = &self.value[..self.cursor];
        let is_sep = |c: char| c.is_whitespace() || matches!(c, '/' | '-' | '_' | '.');
        let trimmed = before.trim_end_matches(char::is_whitespace);
        let mut start = trimmed.rfind(is_sep).map_or(0, |i| i + trimmed[i..].chars().next().map_or(1, char::len_utf8));
        if start == self.cursor {
            // Cursor sits right after a separator: delete just that separator.
            start = self.prev_boundary();
        }
        self.value.drain(start..self.cursor);
        self.cursor = start;
        true
    }

    /// The slice visible in a box `width` columns wide (scrolled so the cursor
    /// stays visible) and the cursor's display column inside it.
    pub fn view(&self, width: usize) -> (String, usize) {
        let width = width.max(1);
        let graphemes: Vec<(usize, &str)> = self.value.grapheme_indices(true).collect();
        // Index of the grapheme the cursor sits before.
        let cur = graphemes.iter().position(|(i, _)| *i >= self.cursor).unwrap_or(graphemes.len());

        // Scroll the start forward until the text up to the cursor (plus one
        // column for the cursor itself) fits.
        let mut start = 0;
        let w_between = |a: usize, b: usize| graphemes[a..b].iter().map(|(_, g)| g.width()).sum::<usize>();
        while start < cur && w_between(start, cur) + 1 > width {
            start += 1;
        }
        let col = w_between(start, cur);

        let mut out = String::new();
        let mut used = 0;
        for (_, g) in &graphemes[start..] {
            let w = g.width();
            if used + w > width {
                break;
            }
            out.push_str(g);
            used += w;
        }
        (out, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn typed(s: &str) -> Input {
        let mut i = Input::default();
        for c in s.chars() {
            assert!(i.handle(&key(KeyCode::Char(c))));
        }
        i
    }

    #[test]
    fn typing_and_backspace() {
        let mut i = typed("abc");
        assert_eq!(i.value(), "abc");
        assert!(i.handle(&key(KeyCode::Backspace)));
        assert_eq!(i.value(), "ab");
        i.handle(&key(KeyCode::Home));
        assert!(!i.handle(&key(KeyCode::Backspace)), "backspace at start is a no-op");
        assert!(i.handle(&key(KeyCode::Delete)));
        assert_eq!(i.value(), "b");
    }

    #[test]
    fn insert_in_middle() {
        let mut i = typed("ac");
        i.handle(&key(KeyCode::Left));
        i.handle(&key(KeyCode::Char('b')));
        assert_eq!(i.value(), "abc");
    }

    #[test]
    fn movement_does_not_report_change() {
        let mut i = typed("ab");
        for k in [KeyCode::Left, KeyCode::Right, KeyCode::Home, KeyCode::End] {
            assert!(!i.handle(&key(k)));
        }
        assert!(!i.handle(&ctrl('a')));
    }

    #[test]
    fn control_chars_are_rejected() {
        let mut i = Input::default();
        assert!(!i.handle(&key(KeyCode::Char('\u{7}'))));
        assert!(i.is_empty());
    }

    #[test]
    fn thai_cluster_moves_as_one_but_backspace_removes_a_mark() {
        // "กี่" = ก + ี (vowel) + ่ (tone mark): one grapheme, three code points.
        let mut i = typed("กี่");
        assert_eq!(i.view(10), ("กี่".to_string(), 1));
        assert!(i.handle(&key(KeyCode::Backspace)));
        assert_eq!(i.value(), "กี");
        i.handle(&key(KeyCode::Left));
        assert_eq!(i.view(10).1, 0, "Left jumps over the whole cluster");
        i.handle(&key(KeyCode::Delete));
        assert!(i.is_empty(), "Delete removes the whole cluster");
    }

    #[test]
    fn wide_chars_use_display_width() {
        let i = typed("日本");
        assert_eq!(i.view(10), ("日本".to_string(), 4));
        // Only 3 columns: must scroll so the cursor column fits.
        let (s, col) = i.view(3);
        assert_eq!(s, "本");
        assert_eq!(col, 2);
    }

    #[test]
    fn view_scrolls_to_cursor() {
        let i = typed("abcdefghij");
        let (s, col) = i.view(5);
        assert_eq!(s, "ghij");
        assert_eq!(col, 4);
    }

    #[test]
    fn ctrl_w_deletes_words_and_path_segments() {
        let mut i = typed("~/projects/my-fw");
        assert!(i.handle(&ctrl('w')));
        assert_eq!(i.value(), "~/projects/my-");
        i.handle(&ctrl('w'));
        assert_eq!(i.value(), "~/projects/my");
        i.handle(&ctrl('w'));
        assert_eq!(i.value(), "~/projects/");
        let mut j = typed("hello world  ");
        j.handle(&ctrl('w'));
        assert_eq!(j.value(), "hello ");
    }

    #[test]
    fn ctrl_u_clears_before_cursor() {
        let mut i = typed("abcd");
        i.handle(&key(KeyCode::Left));
        assert!(i.handle(&ctrl('u')));
        assert_eq!(i.value(), "d");
    }
}
