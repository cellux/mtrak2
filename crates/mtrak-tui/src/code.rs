//! The Code screen: a multiline Lua text editor and a simple lexical
//! highlighter.
//!
//! Editing is intentionally small and deterministic so it can be unit-tested
//! without a terminal. The highlighter covers practical lexical categories
//! only (keywords, function names, numbers, strings, comments, punctuation).

/// A lexical category produced by the highlighter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    /// A Lua keyword such as `local` or `function`.
    Keyword,
    /// A function name following `function` or a `name(` call.
    Function,
    /// A numeric literal.
    Number,
    /// A string literal.
    String,
    /// A comment.
    Comment,
    /// Punctuation.
    Punctuation,
}

/// A highlighted span: `[start, end)` character columns and a category.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    /// Start column (inclusive).
    pub start: usize,
    /// End column (exclusive).
    pub end: usize,
    /// The category.
    pub kind: TokenKind,
}

const KEYWORDS: &[&str] = &[
    "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "goto", "if", "in",
    "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
];

/// A multiline Lua editor with a character cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeEditor {
    lines: Vec<Vec<char>>,
    cursor_line: usize,
    cursor_col: usize,
}

impl Default for CodeEditor {
    fn default() -> Self {
        CodeEditor::from_text("")
    }
}

impl CodeEditor {
    /// Creates an editor over `text`.
    pub fn from_text(text: &str) -> Self {
        let mut lines: Vec<Vec<char>> = text
            .split('\n')
            .map(|line| line.chars().collect())
            .collect();
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        CodeEditor {
            lines,
            cursor_line: 0,
            cursor_col: 0,
        }
    }

    /// The current text, newline-separated.
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|line| line.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The number of lines.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The characters of a line.
    pub fn line(&self, index: usize) -> &[char] {
        self.lines.get(index).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The cursor as `(line, column)`.
    pub fn cursor(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    /// Moves the cursor left, wrapping to the previous line.
    pub fn move_left(&mut self) {
        if self.cursor_col > 0 {
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
        }
    }

    /// Moves the cursor right, wrapping to the next line.
    pub fn move_right(&mut self) {
        if self.cursor_col < self.lines[self.cursor_line].len() {
            self.cursor_col += 1;
        } else if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = 0;
        }
    }

    /// Moves the cursor up.
    pub fn move_up(&mut self) {
        if self.cursor_line > 0 {
            self.cursor_line -= 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    /// Moves the cursor down.
    pub fn move_down(&mut self) {
        if self.cursor_line + 1 < self.lines.len() {
            self.cursor_line += 1;
            self.cursor_col = self.cursor_col.min(self.lines[self.cursor_line].len());
        }
    }

    /// Moves the cursor to the start of the line.
    pub fn move_home(&mut self) {
        self.cursor_col = 0;
    }

    /// Moves the cursor to the end of the line.
    pub fn move_end(&mut self) {
        self.cursor_col = self.lines[self.cursor_line].len();
    }

    /// Inserts a character at the cursor.
    pub fn insert(&mut self, ch: char) {
        self.lines[self.cursor_line].insert(self.cursor_col, ch);
        self.cursor_col += 1;
    }

    /// Splits the line at the cursor.
    pub fn newline(&mut self) {
        let tail = self.lines[self.cursor_line].split_off(self.cursor_col);
        self.lines.insert(self.cursor_line + 1, tail);
        self.cursor_line += 1;
        self.cursor_col = 0;
    }

    /// Deletes the character before the cursor, merging lines at column 0.
    pub fn backspace(&mut self) {
        if self.cursor_col > 0 {
            self.lines[self.cursor_line].remove(self.cursor_col - 1);
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            let current = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].len();
            self.lines[self.cursor_line].extend(current);
        }
    }

    /// Deletes the character at the cursor, merging the next line at the end.
    pub fn delete(&mut self) {
        if self.cursor_col < self.lines[self.cursor_line].len() {
            self.lines[self.cursor_line].remove(self.cursor_col);
        } else if self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].extend(next);
        }
    }

    /// Inserts a tab (two spaces) at the cursor.
    pub fn tab(&mut self) {
        for _ in 0..2 {
            self.insert(' ');
        }
    }
}

/// Highlights one line of Lua into spans.
pub fn highlight(line: &[char]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut i = 0;
    let n = line.len();
    while i < n {
        let ch = line[i];
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        // Comments.
        if ch == '-' && line.get(i + 1) == Some(&'-') {
            spans.push(Span {
                start: i,
                end: n,
                kind: TokenKind::Comment,
            });
            break;
        }
        // Strings.
        if ch == '"' || ch == '\'' {
            let quote = ch;
            let start = i;
            i += 1;
            while i < n {
                if line[i] == '\\' {
                    i += 2;
                    continue;
                }
                if line[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            spans.push(Span {
                start,
                end: i.min(n),
                kind: TokenKind::String,
            });
            continue;
        }
        // Numbers.
        if ch.is_ascii_digit() {
            let start = i;
            while i < n && (line[i].is_ascii_alphanumeric() || line[i] == '.') {
                i += 1;
            }
            spans.push(Span {
                start,
                end: i,
                kind: TokenKind::Number,
            });
            continue;
        }
        // Identifiers and keywords.
        if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < n && (line[i].is_alphanumeric() || line[i] == '_') {
                i += 1;
            }
            let word: String = line[start..i].iter().collect();
            let kind = if KEYWORDS.contains(&word.as_str()) {
                TokenKind::Keyword
            } else if is_function_name(line, i) {
                TokenKind::Function
            } else {
                continue;
            };
            spans.push(Span {
                start,
                end: i,
                kind,
            });
            continue;
        }
        // Punctuation.
        let start = i;
        i += 1;
        spans.push(Span {
            start,
            end: i,
            kind: TokenKind::Punctuation,
        });
    }
    spans
}

fn is_function_name(line: &[char], end: usize) -> bool {
    let mut i = end;
    while i < line.len() && line[i].is_whitespace() {
        i += 1;
    }
    line.get(i) == Some(&'(')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(editor: &CodeEditor) -> String {
        editor.text()
    }

    #[test]
    fn typing_and_navigation_edit_the_text() {
        let mut editor = CodeEditor::from_text("local M = {}");
        editor.move_end();
        editor.insert(';');
        assert_eq!(text_of(&editor), "local M = {};");
        editor.newline();
        editor.insert('x');
        assert_eq!(text_of(&editor), "local M = {};\nx");
        editor.backspace();
        editor.backspace();
        assert_eq!(text_of(&editor), "local M = {};");
        editor.move_up();
        editor.move_end();
        editor.delete();
        assert_eq!(text_of(&editor), "local M = {};");
    }

    #[test]
    fn tab_indents_two_spaces() {
        let mut editor = CodeEditor::from_text("x");
        editor.move_home();
        editor.tab();
        assert_eq!(text_of(&editor), "  x");
    }

    #[test]
    fn highlighting_covers_practical_categories() {
        let line: Vec<char> = "local x = 12 -- note".chars().collect();
        let spans = highlight(&line);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Number));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
        let line: Vec<char> = "function M.run(a) return \"hi\" end".chars().collect();
        let spans = highlight(&line);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Function));
        assert!(spans.iter().any(|s| s.kind == TokenKind::String));
    }

    #[test]
    fn highlight_spans_are_in_bounds_and_ordered() {
        let line: Vec<char> = "local t = { a = 1 }".chars().collect();
        let spans = highlight(&line);
        let mut last = 0;
        for span in spans {
            assert!(span.start < span.end);
            assert!(span.end <= line.len());
            assert!(span.start >= last);
            last = span.end;
        }
    }
}
