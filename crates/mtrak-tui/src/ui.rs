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
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::{Frame, Terminal};

use crate::app::{App, Mode};
use crate::key::{Code, Key};

/// Runs the interactive application until the user quits.
pub fn run(app: &mut App) -> io::Result<()> {
    run_with(app, |_| {})
}

/// Runs the application, calling `on_tick` once per frame for side effects such
/// as re-syncing a project's Lua sources after a reload.
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
    while !app.should_quit() {
        on_tick(app);
        terminal.draw(|frame| draw(frame, app))?;
        if event::poll(Duration::from_millis(16))? {
            match event::read()? {
                CtEvent::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(converted) = convert_key(key) {
                        app.handle_key(converted);
                    }
                }
                CtEvent::Resize(..) => {}
                _ => {}
            }
        }
        let now = Instant::now();
        let delta = now.duration_since(last);
        last = now;
        if app.is_playing() {
            app.tick_playback(delta.as_micros() as u64);
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
    draw_grid(frame, app, chunks[1]);
    draw_status(frame, app, chunks[2]);
}

fn draw_header(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let composition = &app.project().composition;
    let settings = app.effective_settings();
    let note_mode = match app.mode() {
        Mode::Normal => "scal",
        Mode::Note if app.note_chromatic() => "chrom",
        Mode::Note => "scal",
        Mode::Command => "cmd",
        Mode::EventEditor => "evt",
    };
    let line = Line::from(vec![
        Span::styled(" mtrak ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(format!(
            " BPM {}  TPB {}  LPB {}  PAT {}  ROOT {}  NOTE {} ",
            composition.bpm(),
            composition.tpb(),
            app.pattern().lpb(),
            app.current_pattern(),
            mtrak_core::settings::format_note(settings.root),
            note_mode,
        )),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_grid(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
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
    let label_w = 5usize;
    let cell_w = 7usize;
    let visible_tracks = (((inner.width as usize).saturating_sub(label_w)) / cell_w)
        .max(1)
        .min(tracks);
    let visible_rows = (inner.height as usize).saturating_sub(1).max(1);
    let row_off = scroll_offset(app.row, rows, visible_rows);
    let track_off = scroll_offset(app.track, tracks, visible_tracks);
    let selection = app.selection().map(|s| s.rect());
    let affected = app.affected_rect();
    let note_mode = app.mode() == Mode::Note;

    let mut lines: Vec<Line> = Vec::new();
    let mut header = vec![Span::styled(
        format!("{:>4} ", "row"),
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
        let row_style = if lpb != 0 && row % lpb == 0 {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let mut spans = vec![Span::styled(format!("{row:04X} "), row_style)];
        for track in track_off..(track_off + visible_tracks).min(tracks) {
            let text = app.cell_text(row, track);
            let mut style = Style::default();
            let in_selection = selection
                .map(|s| row >= s.top && row <= s.bottom && track >= s.left && track <= s.right)
                .unwrap_or(false);
            if in_selection {
                style = style.bg(Color::Rgb(0, 40, 90));
            } else if row >= affected.top
                && row <= affected.bottom
                && track >= affected.left
                && track <= affected.right
            {
                style = style.bg(Color::Rgb(0, 24, 48));
            }
            if row == app.row && track == app.track {
                let cursor_bg = if note_mode {
                    Color::Magenta
                } else {
                    Color::Blue
                };
                style = style
                    .bg(cursor_bg)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD);
            }
            if app.is_playing() && row == app.play_row() {
                style = style.fg(Color::Green).add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(format!("{text} "), style));
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_status(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let line = match app.mode() {
        Mode::Command => Line::from(vec![
            Span::styled(":", Style::default().fg(Color::Yellow)),
            Span::raw(app.input().to_string()),
        ]),
        Mode::EventEditor => Line::from(vec![
            Span::styled("event ", Style::default().fg(Color::Magenta)),
            Span::raw(app.input().to_string()),
            Span::styled(
                "   [enter commit, esc cancel]",
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        _ => match app.message() {
            Some(message) if message.is_error => Line::from(Span::styled(
                message.text.clone(),
                Style::default().fg(Color::Red),
            )),
            Some(message) => Line::from(Span::styled(
                message.text.clone(),
                Style::default().fg(Color::Green),
            )),
            None => Line::from(Span::styled(
                "space play  s start  : command  ctrl-n note  ctrl-s save  ctrl-q quit",
                Style::default().fg(Color::DarkGray),
            )),
        },
    };
    frame.render_widget(Paragraph::new(line), area);
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
