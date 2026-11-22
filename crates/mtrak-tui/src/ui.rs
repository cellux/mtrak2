//! Terminal rendering and the input/playback loop.
//!
//! This is the only module that knows about `ratatui`; everything it draws and
//! every key it forwards is handled by [`crate::app`].

use std::io;
use std::time::{Duration, Instant};

use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::crossterm::event::{
    self, Event as CtEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect as Area};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{Frame, Terminal};

use crate::app::{App, Mode, Screen};
use crate::chooser::ChooserKind;
use crate::code::{highlight, TokenKind};
use crate::key::{Code, Key};

/// Longest the loop sleeps before polling input again. This caps how long a
/// key press can wait while playback is idle, and bounds redraw latency.
const FRAME: Duration = Duration::from_millis(16);

/// Background used to band the row the engine is currently playing.
const PLAY_ROW_BG: Color = Color::Rgb(0, 64, 40);
/// Foreground for text on the currently playing row.
const PLAY_ROW_FG: Color = Color::White;
/// Marks the row the engine is currently playing, left of the first column.
const PLAY_ROW_MARK: char = '▶';
/// Separates the hex coordinate labels of the chooser grid.
const CHOOSER_MARK: char = '·';
/// Background of the row/column a partially typed chooser byte selects.
const CHOOSER_AXIS_BG: Color = Color::Rgb(24, 48, 72);

/// Runs the interactive application until the user quits.
pub fn run(app: &mut App) -> io::Result<()> {
    run_with(app, |_| {})
}

/// Runs the application, calling `on_tick` once per frame for side effects.
pub fn run_with(app: &mut App, mut on_tick: impl FnMut(&mut App)) -> io::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let result = event_loop(&mut terminal, app, &mut on_tick);
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

fn event_loop<B: Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    on_tick: &mut impl FnMut(&mut App),
) -> io::Result<()> {
    let mut last = Instant::now();
    let mut playing_row = app.play_row();
    let mut dirty = true;
    while !app.should_quit() {
        on_tick(app);
        // Redrawing is what costs time, and it happens *before* the poll, so a
        // slow frame would delay the next event. Only repaint when something
        // visible changed: a key, a resize, or the playing row moving.
        if dirty {
            terminal.draw(|frame| draw(frame, app))?;
            dirty = false;
        }
        // Sleep only as long as the next event allows, so events leave when
        // they are due instead of at the end of the next frame. `FRAME` caps
        // the wait so input stays responsive when nothing is scheduled.
        let timeout = app.next_event_in().map_or(FRAME, |next| next.min(FRAME));
        if event::poll(timeout)? {
            match event::read()? {
                CtEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(converted) = convert_key(key) {
                        app.handle_key(converted);
                    }
                }
                CtEvent::Resize(..) => {}
                _ => {}
            }
            dirty = true;
        }
        let now = Instant::now();
        let delta = now.duration_since(last);
        last = now;
        if app.is_playing() {
            app.tick_playback(delta.as_micros() as u64);
            if app.play_row() != playing_row {
                playing_row = app.play_row();
                dirty = true;
            }
            if !app.is_playing() {
                // The pattern ended: repaint once to clear the playhead.
                dirty = true;
            }
        }
    }
    Ok(())
}

fn convert_key(key: KeyEvent) -> Option<Key> {
    let code = match key.code {
        KeyCode::Char(c) => Code::Char(c),
        KeyCode::Enter => Code::Enter,
        KeyCode::Esc => Code::Esc,
        KeyCode::Backspace => Code::Backspace,
        KeyCode::Delete => Code::Delete,
        KeyCode::Insert => Code::Insert,
        KeyCode::Left => Code::Left,
        KeyCode::Right => Code::Right,
        KeyCode::Up => Code::Up,
        KeyCode::Down => Code::Down,
        KeyCode::PageUp => Code::PageUp,
        KeyCode::PageDown => Code::PageDown,
        KeyCode::Home => Code::Home,
        KeyCode::End => Code::End,
        KeyCode::Tab | KeyCode::BackTab => Code::Tab,
        KeyCode::F(n) => Code::Function(n),
        _ => return None,
    };
    let back_tab = matches!(key.code, KeyCode::BackTab);
    Some(Key {
        code,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        shift: key.modifiers.contains(KeyModifiers::SHIFT) || back_tab,
        alt: key.modifiers.contains(KeyModifiers::ALT),
    })
}

fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);
    draw_header(frame, app, chunks[0]);
    match app.screen() {
        Screen::Help => draw_help(frame, chunks[1]),
        Screen::Pattern => draw_pattern(frame, app, chunks[1]),
        Screen::Code => draw_code(frame, app, chunks[1]),
    }
    if app.mode() == Mode::Chooser {
        draw_chooser(frame, app, area);
    }
    draw_status(frame, app, chunks[2]);
    if app.mode() == Mode::Command {
        draw_completions(frame, app, area);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Area) {
    let composition = &app.project().composition;
    let note_mode = match app.mode() {
        Mode::Note | Mode::Normal if app.note_chromatic() => "chrom",
        Mode::Note | Mode::Normal => "scal",
        Mode::Command => "cmd",
        Mode::Chooser => "choose",
    };
    let screen = match app.screen() {
        Screen::Help => "HELP".to_string(),
        Screen::Pattern => format!("PAT {:02X}", app.current_pattern().value()),
        Screen::Code => format!("MOD {:02X}", app.module_id()),
    };
    let note_style = if app.mode() == Mode::Note {
        Style::default()
            .fg(Color::White)
            .bg(Color::Magenta)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    let line = Line::from(vec![
        Span::styled(" mtrak ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            " BPM {}  TPB {}  {}  ROOT {}  ",
            composition.bpm(),
            composition.tpb(),
            screen,
            mtrak_core::settings::format_note(app.effective_settings().root),
        )),
        Span::styled("NOTE", note_style),
        Span::raw(format!(" {note_mode} ")),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_help(frame: &mut Frame, area: Area) {
    let block = Block::default().borders(Borders::ALL).title(" help ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = vec![
        Line::raw("F1 help   F2 pattern   F3 code"),
        Line::raw(""),
        Line::raw("Pattern: arrows move  hex types  enter edits  alt-enter choose"),
        Line::raw("         o offsets  space play  : command  ctrl-n note  ctrl-s save"),
        Line::raw("         ctrl-f follow the playhead  alt-. all notes off"),
        Line::raw("Code:    alt-enter choose module  F5 reload  ctrl-z undo"),
        Line::raw("Command: tab completes :r and :w paths (type to filter)"),
        Line::raw("         arrows pick, tab/enter use, esc closes"),
        Line::raw("         ctrl-a/e home/end  ctrl-k/u kill  alt-bksp word"),
    ];
    frame.render_widget(Paragraph::new(text), inner);
}

fn draw_pattern(frame: &mut Frame, app: &App, area: Area) {
    let title = format!(" pattern {:02X} ", app.current_pattern().value());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 8 || inner.height < 2 {
        return;
    }

    let pattern = app.pattern();
    let rows = pattern.row_count();
    let tracks = pattern.track_count();
    let lpb = pattern.lpb() as usize;
    let offsets = app.offsets_visible();
    let npc = app.nibbles_per_cell();
    let cell_w = if offsets { 12 } else { 7 };
    // Row number, play marker, and the separating space.
    let label_w = 6usize;
    let visible_tracks = (((inner.width as usize).saturating_sub(label_w)) / cell_w)
        .max(1)
        .min(tracks);
    let visible_rows = (inner.height as usize).saturating_sub(1).max(1);
    let (cursor_row, cursor_track, cursor_nibble) = app.cursor();
    let view_row = if app.is_playing() && app.follow_playhead() {
        app.play_row()
    } else {
        cursor_row
    };
    let row_off = scroll_offset(view_row, rows, visible_rows);
    let track_off = scroll_offset(cursor_track, tracks, visible_tracks);
    let selection = app.selection_bounds();
    let brush = app.brush_rect();
    let note_mode = app.mode() == Mode::Note;
    let cursor_char = if cursor_nibble < 6 {
        cursor_nibble
    } else {
        cursor_nibble + 1
    };

    let mut lines: Vec<Line> = Vec::new();
    let mut header = vec![Span::styled(
        format!("{:>4}  ", "row"),
        Style::default().fg(Color::DarkGray),
    )];
    for track in track_off..(track_off + visible_tracks).min(tracks) {
        header.push(Span::styled(
            format!("{track:02X}     "),
            Style::default().fg(Color::Yellow),
        ));
    }
    lines.push(Line::from(header));

    for row in row_off..(row_off + visible_rows).min(rows) {
        let playing = app.is_playing() && row == app.play_row();
        let row_style = if lpb != 0 && row % lpb == 0 {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let row_style = if playing {
            row_style.bg(PLAY_ROW_BG)
        } else {
            row_style
        };
        let mut spans = vec![Span::styled(format!("{row:04X}"), row_style)];
        let marker_style = if playing {
            Style::default()
                .fg(PLAY_ROW_FG)
                .bg(PLAY_ROW_BG)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(
            if playing {
                PLAY_ROW_MARK.to_string()
            } else {
                " ".to_string()
            },
            marker_style,
        ));
        spans.push(Span::styled(" ", row_style));
        for track in track_off..(track_off + visible_tracks).min(tracks) {
            let text = app.cell_display(row, track);
            let invalid_offset =
                offsets && row == cursor_row && track == cursor_track && app.offset_invalid();
            for (i, ch) in text.chars().enumerate() {
                // Map a character index to its nibble column (`+` is the
                // offset separator and has no column).
                let nibble = if i < 6 {
                    Some(i)
                } else if offsets {
                    if i == 6 {
                        None
                    } else {
                        Some(i - 1)
                    }
                } else {
                    Some(i)
                };
                let column = nibble.map(|n| track * npc + n);
                let in_selection = column
                    .map(|c| selection.map(|s| s.contains(row, c)).unwrap_or(false))
                    .unwrap_or(false);
                let in_brush = column.map(|c| brush.contains(row, c)).unwrap_or(false);
                let mut style = match (in_selection, in_brush) {
                    (true, true) => Style::default().bg(Color::Rgb(0, 56, 108)),
                    (true, false) => Style::default().bg(Color::Rgb(0, 40, 90)),
                    (false, true) => Style::default().bg(Color::Rgb(0, 24, 48)),
                    (false, false) => Style::default(),
                };
                if invalid_offset && i > 6 {
                    style = style.fg(Color::Red);
                }
                if row == cursor_row && track == cursor_track && i == cursor_char {
                    let cursor_bg = if note_mode {
                        Color::Magenta
                    } else {
                        Color::Blue
                    };
                    style = style
                        .bg(cursor_bg)
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD);
                } else if playing {
                    style = style.fg(PLAY_ROW_FG).add_modifier(Modifier::BOLD);
                    // Keep selection and brush backgrounds visible on top of
                    // the playback band.
                    if !in_selection && !in_brush {
                        style = style.bg(PLAY_ROW_BG);
                    }
                }
                spans.push(Span::styled(ch.to_string(), style));
            }
            spans.push(Span::styled(
                " ",
                if playing {
                    Style::default().bg(PLAY_ROW_BG)
                } else {
                    Style::default()
                },
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_code(frame: &mut Frame, app: &App, area: Area) {
    let readonly = app.module_readonly(app.module_id());
    let title = format!(
        " module {:02X}{} ",
        app.module_id(),
        if readonly { " (built-in)" } else { "" }
    );
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 4 || inner.height < 1 {
        return;
    }
    let editor = app.code_editor();
    let (cursor_line, cursor_col) = editor.cursor();
    let visible = inner.height as usize;
    let scroll = viewport_scroll(app.code_scroll(), cursor_line, editor.line_count(), visible);
    let mut lines: Vec<Line> = Vec::new();
    for line_index in scroll..(scroll + visible).min(editor.line_count()) {
        let chars = editor.line(line_index);
        let spans = highlight_line(chars);
        let mut rendered = vec![Span::styled(
            format!("{:>4} ", line_index + 1),
            Style::default().fg(Color::DarkGray),
        )];
        if line_index == cursor_line {
            rendered.extend(cursor_spans(&spans, cursor_col));
        } else {
            rendered.extend(spans);
        }
        lines.push(Line::from(rendered));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn highlight_line(chars: &[char]) -> Vec<Span<'static>> {
    let spans = highlight(chars);
    let mut rendered = Vec::new();
    let mut cursor = 0;
    for span in spans {
        if span.start > cursor {
            let text: String = chars[cursor..span.start].iter().collect();
            rendered.push(Span::raw(text));
        }
        let text: String = chars[span.start..span.end.min(chars.len())]
            .iter()
            .collect();
        rendered.push(Span::styled(text, token_style(span.kind)));
        cursor = span.end;
    }
    if cursor < chars.len() {
        let text: String = chars[cursor..].iter().collect();
        rendered.push(Span::raw(text));
    }
    rendered
}

/// Draws `spans` with the cursor cell highlighted, preserving token colors.
///
/// A cursor at (or past) the end of the line is shown as a reversed space, so
/// empty lines have a visible cursor too.
fn cursor_spans(spans: &[Span<'static>], cursor_col: usize) -> Vec<Span<'static>> {
    let cursor_style = Style::default().bg(Color::Blue).fg(Color::White);
    let mut rendered = Vec::new();
    let mut index = 0usize;
    let mut cursor_drawn = false;
    for span in spans {
        let chars: Vec<char> = span.content.chars().collect();
        let mut chunk_start = 0usize;
        for (i, ch) in chars.iter().enumerate() {
            if index + i == cursor_col {
                if chunk_start < i {
                    let text: String = chars[chunk_start..i].iter().collect();
                    rendered.push(Span::styled(text, span.style));
                }
                rendered.push(Span::styled(ch.to_string(), span.style.patch(cursor_style)));
                chunk_start = i + 1;
                cursor_drawn = true;
            }
        }
        if chunk_start < chars.len() {
            let text: String = chars[chunk_start..].iter().collect();
            rendered.push(Span::styled(text, span.style));
        }
        index += chars.len();
    }
    if !cursor_drawn {
        rendered.push(Span::styled(" ", cursor_style));
    }
    rendered
}

fn token_style(kind: TokenKind) -> Style {
    match kind {
        TokenKind::Keyword => Style::default().fg(Color::Magenta),
        TokenKind::Function => Style::default().fg(Color::Cyan),
        TokenKind::Number => Style::default().fg(Color::Yellow),
        TokenKind::String => Style::default().fg(Color::Green),
        TokenKind::Comment => Style::default().fg(Color::DarkGray),
        TokenKind::Punctuation => Style::default().fg(Color::Gray),
    }
}

fn draw_chooser(frame: &mut Frame, app: &App, area: Area) {
    let Some(chooser) = app.chooser() else {
        return;
    };
    // A row label (`0·`), sixteen two-digit cells with a separating space
    // each, and the popup borders.
    let label_w = 3;
    let width = label_w + 16 * 3 + 2;
    let height = 1 + 16 + 2;
    let popup = centered(area, width as u16, height as u16);
    frame.render_widget(Clear, popup);
    let title = match chooser.kind {
        ChooserKind::Pattern => " patterns ",
        ChooserKind::Code => " modules ",
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let label_style = Style::default().fg(Color::DarkGray);
    let axis_label_style = label_style.fg(Color::Cyan).add_modifier(Modifier::BOLD);
    // The row (and, after the second digit, the column) picked out by the hex
    // digits typed so far.
    let typed = chooser.typed_row_col();
    let mut lines = Vec::new();
    let mut header = vec![Span::styled(" ".repeat(label_w), label_style)];
    for col in 0..16u8 {
        let style = if typed.is_some_and(|(_, column)| column == Some(col)) {
            axis_label_style
        } else {
            label_style
        };
        header.push(Span::styled(format!("{CHOOSER_MARK}{col:X} "), style));
    }
    lines.push(Line::from(header));
    for row in 0..16u8 {
        let in_row = typed.is_some_and(|(typed_row, _)| typed_row == row);
        let mut spans = vec![Span::styled(
            format!("{row:X}{CHOOSER_MARK} "),
            if in_row {
                axis_label_style
            } else {
                label_style
            },
        )];
        for col in 0..16u8 {
            let id = row * 16 + col;
            let (label, style) = chooser_cell(app, chooser.kind, id);
            let mut style = style;
            let in_col = typed.is_some_and(|(_, column)| column == Some(col));
            if in_row || in_col {
                style = style.bg(CHOOSER_AXIS_BG);
            }
            if id == chooser.cursor {
                style = style
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(label, style));
            // Keep the guide band continuous across the column gaps.
            spans.push(Span::styled(
                " ",
                if in_row || in_col {
                    Style::default().bg(CHOOSER_AXIS_BG)
                } else {
                    Style::default()
                },
            ));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn chooser_cell(app: &App, kind: ChooserKind, id: u8) -> (String, Style) {
    match kind {
        ChooserKind::Pattern => {
            if app.pattern_exists(id) {
                (format!("{id:02X}"), Style::default().fg(Color::Green))
            } else {
                ("..".to_string(), Style::default().fg(Color::DarkGray))
            }
        }
        ChooserKind::Code => {
            let style = if id == 0 {
                Style::default().fg(Color::Green)
            } else if app.module_readonly(id) {
                Style::default().fg(Color::Yellow)
            } else if id >= 0x80 {
                Style::default().fg(Color::Red)
            } else if app.module_available(id) {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            let label = if app.module_source(id).is_some() {
                format!("{id:02X}")
            } else {
                "..".to_string()
            };
            (label, style)
        }
    }
}

fn draw_status(frame: &mut Frame, app: &App, area: Area) {
    if app.mode() == Mode::Command {
        let style = if app.command_failed() {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Yellow)
        };
        let text = app.input_text();
        let cursor = app.input_cursor();
        let mut spans = vec![Span::styled(":", style)];
        for (i, ch) in text.chars().enumerate() {
            let mut char_style = style;
            if i == cursor {
                char_style = char_style.add_modifier(Modifier::REVERSED);
            }
            spans.push(Span::styled(ch.to_string(), char_style));
        }
        if cursor >= text.chars().count() {
            spans.push(Span::styled(" ", style.add_modifier(Modifier::REVERSED)));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    if app.mode() == Mode::Chooser {
        let line = Line::from(Span::styled(
            "arrows move  hex jumps  enter accept  esc cancel  ctrl-c/x/v copy/cut/paste",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(Paragraph::new(line), area);
        return;
    }
    let line = match app.message() {
        Some(message) if message.is_error => Line::from(Span::styled(
            message.text.clone(),
            Style::default().fg(Color::Red),
        )),
        Some(message) => Line::from(Span::styled(
            message.text.clone(),
            Style::default().fg(Color::Green),
        )),
        None => {
            let hint = match app.screen() {
                Screen::Help => "F2 pattern  F3 code  ctrl-q quit",
                Screen::Pattern => {
                    "space play  s start  : command  enter edit  alt-enter choose  ctrl-s save"
                }
                Screen::Code => "alt-enter choose module  F5 reload  ctrl-z undo  ctrl-s save",
            };
            Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)))
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

/// Draws the open path-completion menu just above the status line.
fn draw_completions(frame: &mut Frame, app: &App, area: Area) {
    let Some((values, matches, selected)) = app.path_completions() else {
        return;
    };
    if matches.is_empty() {
        return;
    }
    let mut visible = matches.len().min(10);
    let max_height = area.height.saturating_sub(1);
    let height = (visible as u16 + 2).min(max_height);
    if height < 3 {
        return;
    }
    visible = (height - 2) as usize;
    let offset = selected.saturating_sub(visible.saturating_sub(1));
    // Only the rows that fit on screen matter for the width: measuring every
    // candidate would be wasteful in directories with tens of thousands of
    // entries.
    let width = matches
        .iter()
        .skip(offset)
        .take(visible)
        .map(|index| completion_label(&values[*index]).chars().count())
        .max()
        .unwrap_or(0)
        .saturating_add(4) as u16;
    let width = width.max(20).min(area.width);
    if width < 4 {
        return;
    }
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + area.height.saturating_sub(1 + height);
    let popup = Area {
        x,
        y,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title(format!(
        " completions {}/{} ",
        selected + 1,
        matches.len()
    ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let lines: Vec<Line> = matches
        .iter()
        .enumerate()
        .skip(offset)
        .take(visible)
        .map(|(index, value)| {
            let style = if index == selected {
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(Span::styled(completion_label(&values[*value]), style))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The last path component of a completion, keeping a directory's slash.
fn completion_label(value: &str) -> String {
    let trimmed = value.trim_end_matches('/');
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    if value.ends_with('/') {
        format!("{name}/")
    } else {
        name.to_string()
    }
}

fn centered(area: Area, width: u16, height: u16) -> Area {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Area {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

fn scroll_offset(cursor: usize, total: usize, visible: usize) -> usize {
    if total <= visible {
        return 0;
    }
    let half = visible / 2;
    if cursor < half {
        0
    } else if cursor + half >= total {
        total - visible
    } else {
        cursor - half
    }
}

fn viewport_scroll(current: usize, cursor: usize, total: usize, visible: usize) -> usize {
    if total <= visible {
        return 0;
    }
    let max = total - visible;
    if cursor < current {
        cursor
    } else if cursor >= current + visible {
        (cursor + 1 - visible).min(max)
    } else {
        current.min(max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mtrak_core::composition::{Composition, PatternId};
    use ratatui::backend::TestBackend;

    fn app_with(rows: usize, tracks: usize) -> App {
        let mut project = mtrak_midi_io::TrackerProject::new(Composition::new());
        {
            let pattern = project
                .composition
                .pattern_mut(PatternId(0))
                .expect("pattern 0 exists");
            pattern.resize_rows(rows).unwrap();
            pattern.resize_tracks(tracks).unwrap();
        }
        App::new(project)
    }

    #[test]
    fn playing_row_shows_a_marker_and_a_background_band() {
        let mut app = app_with(8, 2);
        app.start_playback();
        assert!(app.is_playing());

        let mut terminal = Terminal::new(TestBackend::new(60, 12)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();

        let (x, y) = (0..buffer.area.width)
            .flat_map(|x| (0..buffer.area.height).map(move |y| (x, y)))
            .find(|&(x, y)| buffer[(x, y)].symbol() == PLAY_ROW_MARK.to_string())
            .expect("the playing row is marked");
        assert_eq!(buffer[(x, y)].bg, PLAY_ROW_BG);
        // The row number to the left is part of the same band.
        assert_eq!(buffer[(x - 1, y)].bg, PLAY_ROW_BG);
        // Only the playing row is banded.
        let other_bg = buffer[(x, y + 1)].bg;
        assert_ne!(other_bg, PLAY_ROW_BG);
    }

    fn screen_rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    #[test]
    fn chooser_renders_hex_row_and_column_labels() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::alt(Code::Enter));
        assert_eq!(app.mode(), Mode::Chooser);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let rows = screen_rows(terminal.backend().buffer());

        assert!(
            rows.iter()
                .any(|line| line.contains("·0 ·1 ·2 ·3 ·4 ·5 ·6 ·7 ·8 ·9 ·A ·B ·C ·D ·E ·F")),
            "column labels: {rows:#?}"
        );
        assert!(
            rows.iter().any(|line| line.contains("0· 00 .. ..")),
            "row labels and cells: {rows:#?}"
        );
        assert!(rows.iter().any(|line| line.contains("F· ")));
    }

    #[test]
    fn typing_a_byte_bands_its_row_and_then_its_column() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::alt(Code::Enter));

        // Coordinates of cell (row, col): the popup is centered, has a border,
        // and a three-cell label before the 3-cell-wide cells.
        let popup = centered(
            Area {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
            53,
            19,
        );
        let cell = |row: u16, col: u16| (popup.x + 1 + 3 + 3 * col, popup.y + 1 + 1 + row);

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        app.handle_key(Key::ch('4'));
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_eq!(buffer[cell(4, 0)].bg, CHOOSER_AXIS_BG, "row 4 is banded");
        assert_eq!(buffer[cell(4, 15)].bg, CHOOSER_AXIS_BG);
        assert_ne!(buffer[cell(3, 0)].bg, CHOOSER_AXIS_BG);
        assert_ne!(
            buffer[cell(0, 15)].bg,
            CHOOSER_AXIS_BG,
            "no column is known yet"
        );

        app.handle_key(Key::ch('F'));
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_eq!(
            buffer[cell(0, 15)].bg,
            CHOOSER_AXIS_BG,
            "column F is banded"
        );
        assert_eq!(buffer[cell(4, 0)].bg, CHOOSER_AXIS_BG, "row 4 stays");
        assert_eq!(
            buffer[cell(4, 15)].bg,
            Color::Blue,
            "the jumped-to cell keeps the cursor highlight"
        );
        assert_ne!(buffer[cell(0, 14)].bg, CHOOSER_AXIS_BG);

        // Moving abandons the typed byte and its bands.
        app.handle_key(Key::new(Code::Right));
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        assert_ne!(buffer[cell(0, 15)].bg, CHOOSER_AXIS_BG);
        assert_ne!(buffer[cell(4, 0)].bg, CHOOSER_AXIS_BG);
    }

    #[test]
    fn follow_playhead_scrolls_the_view_to_the_playing_row() {
        use crate::key::Action;

        let mut app = app_with(64, 2);
        for _ in 0..50 {
            app.apply(Action::MoveDown);
        }
        app.apply(Action::SetStartRow);
        app.apply(Action::Home);
        assert_eq!(app.cursor().0, 0);
        app.handle_key(Key::ctrl(Code::Char('f')));
        assert!(app.follow_playhead());
        app.start_playback();

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let marker_visible = |terminal: &mut Terminal<TestBackend>, app: &App| {
            terminal.draw(|frame| draw(frame, app)).unwrap();
            let buffer = terminal.backend().buffer();
            (0..buffer.area.width)
                .flat_map(|x| (0..buffer.area.height).map(move |y| (x, y)))
                .any(|(x, y)| buffer[(x, y)].symbol() == PLAY_ROW_MARK.to_string())
        };

        assert!(marker_visible(&mut terminal, &app), "playhead is in view");
        app.handle_key(Key::ctrl(Code::Char('f')));
        assert!(!app.follow_playhead());
        assert!(
            !marker_visible(&mut terminal, &app),
            "without follow the view stays at the cursor"
        );
    }

    #[test]
    fn completion_menu_lists_candidates_above_the_status_line() {
        let dir = std::env::temp_dir().join(format!("mtrak-ui-complete-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("aa.mid"), b"").unwrap();
        std::fs::write(dir.join("bb.mid"), b"").unwrap();
        let dir = dir.to_string_lossy().into_owned();

        let mut app = app_with(4, 1);
        app.handle_key(Key::ch(':'));
        for ch in format!("r {dir}/").chars() {
            app.handle_key(Key::ch(ch));
        }
        app.handle_key(Key::new(Code::Tab));

        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        let rows = screen_rows(buffer);
        assert!(
            rows.iter().any(|line| line.contains("completions")),
            "menu title: {rows:#?}"
        );
        let row = rows
            .iter()
            .position(|line| line.contains("aa.mid"))
            .expect("first candidate row");
        assert!(rows.iter().any(|line| line.contains("bb.mid")));
        let x = rows[row].find("aa.mid").unwrap() as u16;
        assert_eq!(
            buffer[(x, row as u16)].bg,
            Color::Blue,
            "the highlighted candidate is selected"
        );
        // The status line is still drawn below the menu.
        assert!(rows[23].starts_with(':'));
    }

    #[test]
    fn note_mode_highlights_the_header_label() {
        let mut app = app_with(4, 2);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let note_cell = |terminal: &mut Terminal<TestBackend>, app: &App| {
            terminal.draw(|frame| draw(frame, app)).unwrap();
            let buffer = terminal.backend().buffer();
            let x = screen_rows(buffer)[0].find("NOTE").expect("header label") as u16;
            (0..4).map(|i| buffer[(x + i, 0)].bg).collect::<Vec<_>>()
        };

        assert!(note_cell(&mut terminal, &app)
            .iter()
            .all(|bg| *bg != Color::Magenta));

        app.handle_key(Key::ctrl(Code::Char('n')));
        assert_eq!(app.mode(), Mode::Note);
        assert!(note_cell(&mut terminal, &app)
            .iter()
            .all(|bg| *bg == Color::Magenta));
    }

    #[test]
    fn header_shows_the_note_entry_mode_in_normal_and_note_modes() {
        let mut app = app_with(4, 2);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let header = |terminal: &mut Terminal<TestBackend>, app: &App| {
            terminal.draw(|frame| draw(frame, app)).unwrap();
            screen_rows(terminal.backend().buffer())[0].clone()
        };

        assert!(header(&mut terminal, &app).contains("NOTE scal"));

        // Shift-N toggles chromatic entry even outside note mode; the header
        // must not keep claiming scale entry.
        app.handle_key(Key::ch('N'));
        assert!(header(&mut terminal, &app).contains("NOTE chrom"));

        app.handle_key(Key::ctrl(Code::Char('n')));
        assert_eq!(app.mode(), Mode::Note);
        assert!(header(&mut terminal, &app).contains("NOTE chrom"));

        app.handle_key(Key::ch('N'));
        assert!(header(&mut terminal, &app).contains("NOTE scal"));
    }

    fn cells_with_bg(buffer: &ratatui::buffer::Buffer, bg: Color) -> Vec<(u16, u16)> {
        (0..buffer.area.width)
            .flat_map(|x| (0..buffer.area.height).map(move |y| (x, y)))
            .filter(|&(x, y)| buffer[(x, y)].bg == bg)
            .collect()
    }

    #[test]
    fn code_cursor_is_visible_on_empty_lines_without_highlighting_the_line() {
        let mut app = app_with(4, 2);
        app.handle_key(Key::new(Code::Function(3)));
        assert_eq!(app.screen(), Screen::Code);
        let mut terminal = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let render = |terminal: &mut Terminal<TestBackend>, app: &App| {
            terminal.draw(|frame| draw(frame, app)).unwrap();
            terminal.backend().buffer().clone()
        };

        // On the `local M = {}` line the cursor sits on `l` and the rest of
        // the line keeps its keyword color.
        let buffer = render(&mut terminal, &app);
        let cursor = cells_with_bg(&buffer, Color::Blue);
        assert_eq!(cursor.len(), 1, "exactly one cursor cell");
        let (x, y) = cursor[0];
        assert_eq!(buffer[(x, y)].symbol(), "l");
        assert_eq!(buffer[(x + 1, y)].symbol(), "o");
        assert_eq!(
            buffer[(x + 1, y)].fg,
            Color::Magenta,
            "the cursor line keeps syntax colors"
        );

        // The second line of the module template is empty; the cursor must
        // still be drawn there, without lighting up the whole line.
        app.handle_key(Key::new(Code::Down));
        let buffer = render(&mut terminal, &app);
        let cursor = cells_with_bg(&buffer, Color::Blue);
        assert_eq!(cursor.len(), 1, "the cursor is visible on an empty line");
        assert_eq!(cursor[0], (x, y + 1));
        assert_eq!(buffer[cursor[0]].symbol(), " ");
    }
}
