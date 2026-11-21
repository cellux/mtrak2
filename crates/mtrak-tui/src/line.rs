//! A single-line text buffer with a character cursor.
//!
//! Command mode uses this for standard line editing: Left/Right and Home/End
//! movement, insertion at the cursor, and Backspace/Delete at the cursor.

/// A single-line editor over `char`s.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LineEditor {
    chars: Vec<char>,
    cursor: usize,
    /// The text removed by the most recent kill, for [`LineEditor::yank`].
    killed: String,
}

impl LineEditor {
    /// An empty editor.
    pub fn new() -> Self {
        LineEditor::default()
    }

    /// An editor over `text`, with the cursor at the start.
    pub fn from_text(text: &str) -> Self {
        LineEditor {
            chars: text.chars().collect(),
            cursor: 0,
            killed: String::new(),
        }
    }

    /// The current text.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// The cursor position in characters.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Places the cursor at `position`, clamped to the buffer length.
    pub fn set_cursor(&mut self, position: usize) {
        self.cursor = position.min(self.chars.len());
    }

    /// Clears the buffer.
    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Moves the cursor left.
    pub fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Moves the cursor right.
    pub fn move_right(&mut self) {
        if self.cursor < self.chars.len() {
            self.cursor += 1;
        }
    }

    /// Moves the cursor to the start.
    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    /// Moves the cursor to the end.
    pub fn move_end(&mut self) {
        self.cursor = self.chars.len();
    }

    /// Inserts a character at the cursor.
    pub fn insert(&mut self, ch: char) {
        self.chars.insert(self.cursor, ch);
        self.cursor += 1;
    }

    /// Deletes the character before the cursor.
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.chars.remove(self.cursor - 1);
            self.cursor -= 1;
        }
    }

    /// Deletes the character at the cursor.
    pub fn delete(&mut self) {
        if self.cursor < self.chars.len() {
            self.chars.remove(self.cursor);
        }
    }

    /// Moves to the start of the previous word.
    pub fn move_word_left(&mut self) {
        let mut index = self.cursor;
        while index > 0 && !is_word_char(self.chars[index - 1]) {
            index -= 1;
        }
        while index > 0 && is_word_char(self.chars[index - 1]) {
            index -= 1;
        }
        self.cursor = index;
    }

    /// Moves past the end of the next word.
    pub fn move_word_right(&mut self) {
        let mut index = self.cursor;
        while index < self.chars.len() && !is_word_char(self.chars[index]) {
            index += 1;
        }
        while index < self.chars.len() && is_word_char(self.chars[index]) {
            index += 1;
        }
        self.cursor = index;
    }

    /// Deletes from the cursor to the end of the line.
    pub fn kill_to_end(&mut self) {
        let removed: String = self.chars.drain(self.cursor..).collect();
        self.remember_kill(removed);
    }

    /// Deletes from the start of the line to the cursor.
    pub fn kill_to_start(&mut self) {
        let removed: String = self.chars.drain(..self.cursor).collect();
        self.cursor = 0;
        self.remember_kill(removed);
    }

    /// Deletes the word before the cursor.
    pub fn kill_word_backward(&mut self) {
        self.kill_word_backward_from(0);
    }

    /// Deletes the word before the cursor, but not before `floor`.
    pub fn kill_word_backward_from(&mut self, floor: usize) {
        let end = self.cursor;
        let floor = floor.min(end);
        let mut start = end;
        while start > floor && !is_word_char(self.chars[start - 1]) {
            start -= 1;
        }
        while start > floor && is_word_char(self.chars[start - 1]) {
            start -= 1;
        }
        let removed: String = self.chars.drain(start..end).collect();
        self.cursor = start;
        self.remember_kill(removed);
    }

    /// Deletes the word after the cursor.
    pub fn kill_word_forward(&mut self) {
        let start = self.cursor;
        self.move_word_right();
        let end = self.cursor;
        self.cursor = start;
        let removed: String = self.chars.drain(start..end).collect();
        self.remember_kill(removed);
    }

    /// Deletes the whitespace-delimited word before the cursor.
    pub fn kill_whitespace_word_backward(&mut self) {
        let end = self.cursor;
        let mut start = end;
        while start > 0 && self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        while start > 0 && !self.chars[start - 1].is_whitespace() {
            start -= 1;
        }
        let removed: String = self.chars.drain(start..end).collect();
        self.cursor = start;
        self.remember_kill(removed);
    }

    /// Inserts the most recently killed text at the cursor.
    pub fn yank(&mut self) {
        if self.killed.is_empty() {
            return;
        }
        let text: Vec<char> = self.killed.chars().collect();
        let length = text.len();
        self.chars.splice(self.cursor..self.cursor, text);
        self.cursor += length;
    }

    /// Exchanges the characters around the cursor.
    pub fn transpose(&mut self) {
        let length = self.chars.len();
        if length < 2 {
            return;
        }
        if self.cursor == 0 {
            self.chars.swap(0, 1);
            self.cursor = 2;
        } else if self.cursor < length {
            self.chars.swap(self.cursor - 1, self.cursor);
            self.cursor += 1;
        } else {
            self.chars.swap(length - 2, length - 1);
            self.cursor = length;
        }
    }

    /// The text removed by the most recent kill.
    pub fn killed(&self) -> &str {
        &self.killed
    }

    fn remember_kill(&mut self, text: String) {
        if !text.is_empty() {
            self.killed = text;
        }
    }
}

/// Whether `c` belongs to a word for Emacs-style movement and kills.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_movement_and_kills() {
        let mut line = LineEditor::from_text("read /tmp/song.mid");
        line.move_end();
        line.move_word_left();
        assert_eq!(line.cursor(), "read /tmp/song.".chars().count());
        line.kill_word_backward();
        assert_eq!(line.text(), "read /tmp/mid");
        assert_eq!(line.killed(), "song.");
        line.yank();
        assert_eq!(line.text(), "read /tmp/song.mid");

        let mut line = LineEditor::from_text("abc def");
        line.move_home();
        line.move_word_right();
        assert_eq!(line.cursor(), 3);
        line.kill_word_forward();
        assert_eq!(line.text(), "abc");
    }

    #[test]
    fn whitespace_word_kill_removes_whole_arguments() {
        let mut line = LineEditor::from_text("r /tmp/a /tmp/b.mid");
        line.move_end();
        line.kill_whitespace_word_backward();
        assert_eq!(line.text(), "r /tmp/a ");
        assert_eq!(line.killed(), "/tmp/b.mid");
        line.kill_whitespace_word_backward();
        assert_eq!(line.text(), "r ");
        assert_eq!(line.killed(), "/tmp/a ");
    }

    #[test]
    fn kill_to_start_end_and_yank() {
        let mut line = LineEditor::from_text("abcdef");
        line.set_cursor(3);
        line.kill_to_end();
        assert_eq!(line.text(), "abc");
        assert_eq!(line.killed(), "def");
        line.kill_to_start();
        assert_eq!(line.text(), "");
        assert_eq!(line.killed(), "abc");
        line.yank();
        assert_eq!(line.text(), "abc");

        // Killing an empty region keeps the previous kill.
        let mut line = LineEditor::from_text("xyz");
        line.set_cursor(0);
        line.kill_to_start();
        assert_eq!(line.killed(), "");
        line.kill_to_end();
        assert_eq!(line.killed(), "xyz");
        line.set_cursor(0);
        line.kill_to_start();
        assert_eq!(line.killed(), "xyz", "empty kills do not clobber");
    }

    #[test]
    fn transpose_swaps_around_the_cursor() {
        let mut line = LineEditor::from_text("abc");
        line.set_cursor(2);
        line.transpose();
        assert_eq!(line.text(), "acb");
        assert_eq!(line.cursor(), 3);

        let mut line = LineEditor::from_text("ab");
        line.move_home();
        line.transpose();
        assert_eq!(line.text(), "ba");
        assert_eq!(line.cursor(), 2);

        let mut line = LineEditor::from_text("a");
        line.move_end();
        line.transpose();
        assert_eq!(line.text(), "a");
    }

    #[test]
    fn insertion_and_cursor_movement() {
        let mut line = LineEditor::from_text(":event 90643F");
        line.move_end();
        line.insert(' ');
        assert_eq!(line.text(), ":event 90643F ");
        line.move_home();
        assert_eq!(line.cursor(), 0);
        line.move_right();
        line.delete();
        assert_eq!(line.text(), ":vent 90643F ");
        line.backspace();
        assert_eq!(line.text(), "vent 90643F ");
    }
}
