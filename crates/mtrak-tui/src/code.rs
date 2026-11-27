//! The Code screen: a multiline Fennel text editor and a simple lexical
//! highlighter.
//!
//! Editing is intentionally small and deterministic so it can be unit-tested
//! without a terminal. The highlighter covers practical lexical categories
//! only (special forms/macros, symbols, numbers, strings, comments, and
//! delimiters). Compilation remains the authoritative validation of source
//! correctness; highlighting is presentation only.

/// A lexical category produced by the highlighter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TokenKind {
    /// A Fennel special form or built-in macro such as `fn` or `let`.
    Keyword,
    /// An identifier or symbol.
    Symbol,
    /// A numeric literal.
    Number,
    /// A string literal (including `:keyword` strings).
    String,
    /// A comment.
    Comment,
    /// Punctuation or a delimiter.
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

/// Fennel special forms and built-in macros worth highlighting distinctly.
const KEYWORDS: &[&str] = &[
    "and",
    "band",
    "bnot",
    "bor",
    "bxor",
    "case",
    "case-try",
    "catch",
    "collect",
    "comment",
    "do",
    "doto",
    "each",
    "faccumulate",
    "fcollect",
    "fn",
    "for",
    "global",
    "icollect",
    "if",
    "import-macros",
    "include",
    "lambda",
    "let",
    "local",
    "lshift",
    "macro",
    "macros",
    "match",
    "match-try",
    "nil",
    "not",
    "or",
    "pick-args",
    "require",
    "require-macros",
    "rshift",
    "set",
    "true",
    "false",
    "unless",
    "values",
    "var",
    "when",
    "where",
    "while",
    "with-open",
    "->",
    "->>",
    "?.",
    "?->",
    "?->>",
    "\u{3bb}",
];

/// A multiline Fennel editor with a character cursor.
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

/// Whether `ch` may appear inside a Fennel symbol (or keyword) token.
///
/// Fennel symbols are far more permissive than Lua identifiers: they may
/// contain `-`, `?`, `!`, `*`, `+`, `/`, `<`, `>`, `=`, `&`, `|`, `.`, `:`,
/// and `#`. Recognizing the whole run keeps highlighting stable while typing.
fn is_symbol_char(ch: char) -> bool {
    ch.is_alphanumeric()
        || matches!(
            ch,
            '_' | '-'
                | '?'
                | '!'
                | '*'
                | '+'
                | '/'
                | '<'
                | '>'
                | '='
                | '&'
                | '|'
                | '.'
                | ':'
                | '#'
                | '$'
                | '%'
                | '@'
        )
}

/// Whether `ch` can begin a Fennel symbol token.
fn is_symbol_start(ch: char) -> bool {
    is_symbol_char(ch) && !ch.is_ascii_digit()
}

/// Highlights one line of Fennel into spans.
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
        // Comments run to the end of the line.
        if ch == ';' {
            spans.push(Span {
                start: i,
                end: n,
                kind: TokenKind::Comment,
            });
            break;
        }
        // Strings.
        if ch == '"' {
            let start = i;
            i += 1;
            while i < n {
                if line[i] == '\\' {
                    i += 2;
                    continue;
                }
                if line[i] == '"' {
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
        // Numbers (including hex, floats, and a leading sign).
        let signed = (ch == '-' || ch == '+') && line.get(i + 1).is_some_and(char::is_ascii_digit);
        if ch.is_ascii_digit() || signed {
            let start = i;
            if signed {
                i += 1;
            }
            while i < n
                && (line[i].is_ascii_alphanumeric()
                    || line[i] == '.'
                    || line[i] == '_'
                    || ((line[i] == '+' || line[i] == '-') && matches!(line[i - 1], 'e' | 'E')))
            {
                i += 1;
            }
            spans.push(Span {
                start,
                end: i,
                kind: TokenKind::Number,
            });
            continue;
        }
        // Keyword strings such as `:root`.
        if ch == ':' && line.get(i + 1).is_some_and(|c| is_symbol_start(*c)) {
            let start = i;
            i += 1;
            while i < n && is_symbol_char(line[i]) {
                i += 1;
            }
            spans.push(Span {
                start,
                end: i,
                kind: TokenKind::String,
            });
            continue;
        }
        // Symbols, special forms, and built-in macros.
        if is_symbol_start(ch) {
            let start = i;
            while i < n && is_symbol_char(line[i]) {
                i += 1;
            }
            let word: String = line[start..i].iter().collect();
            let kind = if KEYWORDS.contains(&word.as_str()) {
                TokenKind::Keyword
            } else {
                TokenKind::Symbol
            };
            spans.push(Span {
                start,
                end: i,
                kind,
            });
            continue;
        }
        // Delimiters and other punctuation.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(editor: &CodeEditor) -> String {
        editor.text()
    }

    #[test]
    fn typing_and_navigation_edit_the_text() {
        let mut editor = CodeEditor::from_text("(local M {})");
        editor.move_end();
        editor.insert(';');
        assert_eq!(text_of(&editor), "(local M {});");
        editor.newline();
        editor.insert('x');
        assert_eq!(text_of(&editor), "(local M {});\nx");
        editor.backspace();
        editor.backspace();
        assert_eq!(text_of(&editor), "(local M {});");
        editor.move_up();
        editor.move_end();
        editor.delete();
        assert_eq!(text_of(&editor), "(local M {});");
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
        let line: Vec<char> = "(fn run [a1] ; note".chars().collect();
        let spans = highlight(&line);
        assert!(spans.iter().any(|s| s.kind == TokenKind::Keyword));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Symbol));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Comment));
        assert!(spans.iter().any(|s| s.kind == TokenKind::Punctuation));

        let line: Vec<char> = "(get-setting :root 0x2A 1.5)".chars().collect();
        let spans = highlight(&line);
        assert!(spans.iter().any(|s| s.kind == TokenKind::String));
        assert_eq!(
            spans.iter().filter(|s| s.kind == TokenKind::Number).count(),
            2
        );
    }

    #[test]
    fn highlight_spans_are_in_bounds_and_ordered() {
        let line: Vec<char> = "(let [x 1] (+ x 2))".chars().collect();
        let spans = highlight(&line);
        let mut last = 0;
        for span in spans {
            assert!(span.start < span.end);
            assert!(span.end <= line.len());
            assert!(span.start >= last);
            last = span.end;
        }
    }

    #[test]
    fn kebab_case_symbols_stay_one_token() {
        let line: Vec<char> = "(emit-sysex [1 2])".chars().collect();
        let spans = highlight(&line);
        let symbol = spans
            .iter()
            .find(|s| s.kind == TokenKind::Symbol)
            .expect("a symbol span");
        let text: String = line[symbol.start..symbol.end].iter().collect();
        assert_eq!(text, "emit-sysex");
    }
}
