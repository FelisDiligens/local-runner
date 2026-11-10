use std::io::{self, Stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::style::Print;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use crate::config::Args;
use crate::logger::{LogBuffer, LogControl};
use crate::worker::Worker;
use crate::{dispatch_command, ipc};

/// How often the TUI wakes up to poll IPC messages, drain the log buffer and redraw.
const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Command words that can be completed in the TUI input (tags are not completed).
const COMMANDS: &[&str] = &[
    "start", "stop", "restart", "status", "shutdown", "quit", "exit",
];

/// Restores the terminal to its previous state when dropped,
/// even if the TUI loop panics.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> io::Result<(Self, Terminal<CrosstermBackend<Stdout>>)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok((Self, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

pub(crate) struct App {
    pub(crate) input: String,
    /// Number of lines scrolled up from the bottom (0 = follow the tail).
    scroll: u16,
    lines: Vec<Line<'static>>,
    /// Command history, kept in memory only for the lifetime of the process.
    history: Vec<String>,
    /// Position in `history` while navigating with Up/Down.
    /// `None` means a fresh input line is being edited.
    history_index: Option<usize>,
    /// The in-progress input, stashed while navigating the history.
    stashed_input: String,
    /// Service names from the config, used for tab completion.
    services: Vec<String>,
    /// Cursor position in `input` as a byte offset, always on a char boundary.
    pub(crate) cursor: usize,
}

impl App {
    pub(crate) fn new(services: Vec<String>) -> Self {
        Self {
            input: String::new(),
            scroll: 0,
            lines: Vec::new(),
            history: Vec::new(),
            history_index: None,
            stashed_input: String::new(),
            services,
            cursor: 0,
        }
    }

    /// Tab-completes the current input. Returns false when there is nothing
    /// to complete (the caller rings the bell in that case).
    fn tab_complete(&mut self) -> bool {
        let Some(completed) = complete(&self.input, &self.services) else {
            return false;
        };
        self.input = completed;
        self.cursor = self.input.len();
        true
    }

    /// Clears the input line and resets the cursor.
    fn clear_input(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    /// Inserts a character at the cursor.
    pub(crate) fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    /// Deletes the character before the cursor (Backspace).
    pub(crate) fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let prev = self.prev_char_boundary(self.cursor);
        self.input.drain(prev..self.cursor);
        self.cursor = prev;
    }

    /// Deletes the character at the cursor (Delete).
    pub(crate) fn delete_char(&mut self) {
        if self.cursor < self.input.len() {
            let next = self.next_char_boundary(self.cursor);
            self.input.drain(self.cursor..next);
        }
    }

    /// Deletes everything before the cursor (Ctrl+U).
    pub(crate) fn delete_to_start(&mut self) {
        self.input.drain(..self.cursor);
        self.cursor = 0;
    }

    /// Deletes the word before the cursor (Ctrl+W, Ctrl/Alt+Backspace).
    pub(crate) fn delete_word_back(&mut self) {
        let old_cursor = self.cursor;
        self.word_left();
        self.input.drain(self.cursor..old_cursor);
    }

    pub(crate) fn cursor_left(&mut self) {
        if self.cursor > 0 {
            self.cursor = self.prev_char_boundary(self.cursor);
        }
    }

    pub(crate) fn cursor_right(&mut self) {
        if self.cursor < self.input.len() {
            self.cursor = self.next_char_boundary(self.cursor);
        }
    }

    pub(crate) fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    pub(crate) fn cursor_end(&mut self) {
        self.cursor = self.input.len();
    }

    /// Moves the cursor to the start of the current or previous word (Alt+B, Ctrl+Left).
    pub(crate) fn word_left(&mut self) {
        let mut chars: Vec<(usize, char)> = self
            .input
            .char_indices()
            .take_while(|(i, _)| *i < self.cursor)
            .collect();
        while let Some(&(_, c)) = chars.last() {
            if c.is_whitespace() {
                chars.pop();
            } else {
                break;
            }
        }
        while let Some(&(_, c)) = chars.last() {
            if !c.is_whitespace() {
                chars.pop();
            } else {
                break;
            }
        }
        self.cursor = chars
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
    }

    /// Moves the cursor to the end of the next word (Alt+F, Ctrl+Right).
    pub(crate) fn word_right(&mut self) {
        let after: Vec<(usize, char)> = self
            .input
            .char_indices()
            .filter(|(i, _)| *i >= self.cursor)
            .collect();
        if after.is_empty() {
            return;
        }
        let mut idx = 0;
        while idx < after.len() && after[idx].1.is_whitespace() {
            idx += 1;
        }
        while idx < after.len() && !after[idx].1.is_whitespace() {
            idx += 1;
        }
        self.cursor = if idx > 0 {
            after[idx - 1].0 + after[idx - 1].1.len_utf8()
        } else {
            self.input.len()
        };
    }

    fn prev_char_boundary(&self, mut i: usize) -> usize {
        i -= 1;
        while i > 0 && !self.input.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    fn next_char_boundary(&self, mut i: usize) -> usize {
        i += 1;
        while i < self.input.len() && !self.input.is_char_boundary(i) {
            i += 1;
        }
        i
    }

    /// Records a submitted command in the history (skipping consecutive duplicates).
    fn record_history(&mut self, command: &str) {
        if self.history.last().map(String::as_str) != Some(command) {
            self.history.push(command.to_string());
        }
        self.history_index = None;
    }

    /// Moves one entry back in the history (Up), stashing the in-progress input.
    fn history_up(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let index = match self.history_index {
            None => {
                self.stashed_input = self.input.clone();
                self.history.len() - 1
            }
            Some(0) => return,
            Some(i) => i - 1,
        };
        self.history_index = Some(index);
        self.input = self.history[index].clone();
        self.cursor = self.input.len();
    }

    /// Moves one entry forward in the history (Down); past the newest entry
    /// restores the stashed in-progress input.
    fn history_down(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 < self.history.len() {
            self.history_index = Some(index + 1);
            self.input = self.history[index + 1].clone();
        } else {
            self.history_index = None;
            self.input = self.stashed_input.clone();
        }
        self.cursor = self.input.len();
    }

    fn push_line(&mut self, level: log::Level, message: String) {
        let color = match level {
            log::Level::Error => Color::Red,
            log::Level::Warn => Color::Yellow,
            log::Level::Info => Color::Reset,
            log::Level::Debug => Color::Gray,
            log::Level::Trace => Color::DarkGray,
        };
        for line in message.split('\n') {
            self.lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(color),
            )));
        }
    }
}

/// Completes `input` against commands and service names using the longest
/// common prefix. Returns None when there is nothing to complete.
/// Only the first two words are considered: the first word completes against
/// `COMMANDS`, the second word against `services` (for start/stop/restart).
pub(crate) fn complete(input: &str, services: &[String]) -> Option<String> {
    match input.find(char::is_whitespace) {
        None => complete_command(input),
        Some(i) => complete_service(&input[..i], &input[i..], services),
    }
}

/// Completes a command word. A unique match gets a trailing space so the
/// user can directly type a service name.
fn complete_command(word: &str) -> Option<String> {
    let matches: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(word))
        .collect();
    match matches.len() {
        0 => None,
        1 => Some(format!("{} ", matches[0])),
        _ => {
            let prefix = longest_common_prefix(&matches);
            (prefix.len() > word.len()).then(|| prefix.to_string())
        }
    }
}

/// Completes the service name after start/stop/restart.
fn complete_service(command: &str, rest: &str, services: &[String]) -> Option<String> {
    if !matches!(command, "start" | "stop" | "restart") {
        return None;
    }
    let ws_len = rest.len() - rest.trim_start().len();
    let (ws, partial) = (&rest[..ws_len], &rest[ws_len..]);
    let matches: Vec<&str> = services
        .iter()
        .map(String::as_str)
        .filter(|s| s.starts_with(partial))
        .collect();
    match matches.len() {
        0 => None,
        1 => Some(format!("{command}{ws}{}", matches[0])),
        _ => {
            let prefix = longest_common_prefix(&matches);
            (prefix.len() > partial.len()).then(|| format!("{command}{ws}{prefix}"))
        }
    }
}

fn longest_common_prefix<'a>(candidates: &[&'a str]) -> &'a str {
    let first = candidates[0];
    let mut len = first.len();
    for candidate in &candidates[1..] {
        let mut n = 0;
        while n < len && n < candidate.len() && first.as_bytes()[n] == candidate.as_bytes()[n] {
            n += 1;
        }
        len = n;
    }
    while !first.is_char_boundary(len) {
        len -= 1;
    }
    &first[..len]
}

/// Runs the TUI on the current (main) thread until the worker stops or finishes.
/// Returns Err only if the terminal couldn't be initialized.
pub fn run(
    worker: &Worker,
    args: &Args,
    ctrl_c_pressed: &Arc<AtomicBool>,
    log_buffer: &LogBuffer,
    log_control: &LogControl,
    services: &[String],
) -> io::Result<()> {
    // Suppress stdout logging before entering raw mode, so no log line can be
    // written to the terminal in raw mode (where '\n' doesn't carriage-return).
    log_control.set_tui_active(true);
    let (_guard, mut terminal) = match TerminalGuard::new() {
        Ok(ok) => ok,
        Err(e) => {
            log_control.set_tui_active(false);
            return Err(e);
        }
    };
    let result = run_loop(
        &mut terminal,
        worker,
        args,
        ctrl_c_pressed,
        log_buffer,
        services,
    );
    log_control.set_tui_active(false);
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    worker: &Worker,
    args: &Args,
    ctrl_c_pressed: &Arc<AtomicBool>,
    log_buffer: &LogBuffer,
    services: &[String],
) -> io::Result<()> {
    let mut app = App::new(services.to_vec());
    let mut ipc_errored = false;
    loop {
        if event::poll(TICK_INTERVAL)?
            && let Ok(Event::Key(key)) = event::read()
            && key.kind == KeyEventKind::Press
            && handle_key(&mut app, key, worker)
        {
            break;
        }
        if ctrl_c_pressed.load(Ordering::Relaxed) {
            worker.stop();
        }
        match ipc::read_message(&args.path) {
            Ok(Some(message)) => {
                dispatch_command(&message, worker);
                // Truncate IPC file after processing its message:
                let _ = ipc::write_message("", &args.path);
                ipc_errored = false;
            }
            Err(e) => {
                if !ipc_errored {
                    log::error!("Couldn't read ipc file: {e}");
                    ipc_errored = true;
                }
            }
            _ => {}
        }
        for (level, message) in log_buffer.drain() {
            app.push_line(level, message);
        }
        terminal.draw(|frame| draw(frame, &app))?;
        if worker.is_finished() || worker.is_stopped() {
            break;
        }
    }
    // Make sure the worker is stopped when quitting via "quit"/"exit"/Ctrl+C key:
    worker.stop();
    Ok(())
}

/// Handles a key press. Returns true when the TUI should quit.
fn handle_key(app: &mut App, key: KeyEvent, worker: &Worker) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        // In raw mode Ctrl+C arrives as a key event instead of SIGINT:
        KeyCode::Char('c') if ctrl => {
            worker.stop();
            true
        }
        KeyCode::Enter => {
            let command = app.input.trim().to_string();
            app.clear_input();
            app.stashed_input.clear();
            if matches!(command.as_str(), "quit" | "exit") {
                app.record_history(&command);
                worker.stop();
                return true;
            }
            if !command.is_empty() {
                app.record_history(&command);
                dispatch_command(&command, worker);
            }
            false
        }
        KeyCode::Tab => {
            if !app.tab_complete() {
                // Ring the terminal bell when there is nothing to complete:
                let _ = execute!(io::stdout(), Print("\x07"));
            }
            false
        }
        // Cursor movement and line editing (readline-style):
        KeyCode::Left if ctrl || alt => {
            app.word_left();
            false
        }
        KeyCode::Right if ctrl || alt => {
            app.word_right();
            false
        }
        KeyCode::Left => {
            app.cursor_left();
            false
        }
        KeyCode::Right => {
            app.cursor_right();
            false
        }
        KeyCode::Backspace if ctrl || alt => {
            app.delete_word_back();
            false
        }
        KeyCode::Backspace => {
            app.backspace();
            false
        }
        KeyCode::Delete => {
            app.delete_char();
            false
        }
        KeyCode::Char('b') if ctrl => {
            app.cursor_left();
            false
        }
        KeyCode::Char('f') if ctrl => {
            app.cursor_right();
            false
        }
        KeyCode::Char('a') if ctrl => {
            app.cursor_home();
            false
        }
        KeyCode::Char('e') if ctrl => {
            app.cursor_end();
            false
        }
        KeyCode::Char('w') if ctrl => {
            app.delete_word_back();
            false
        }
        KeyCode::Char('u') if ctrl => {
            app.delete_to_start();
            false
        }
        KeyCode::Char('b') if alt => {
            app.word_left();
            false
        }
        KeyCode::Char('f') if alt => {
            app.word_right();
            false
        }
        KeyCode::Char(c) => {
            app.insert_char(c);
            false
        }
        // Up/Down navigate the command history; the log view is scrolled
        // with PageUp/PageDown/Home/End:
        KeyCode::Up => {
            app.history_up();
            false
        }
        KeyCode::Down => {
            app.history_down();
            false
        }
        KeyCode::PageUp => {
            app.scroll = app.scroll.saturating_add(10);
            false
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_sub(10);
            false
        }
        KeyCode::Home => {
            app.scroll = u16::MAX;
            false
        }
        KeyCode::End => {
            app.scroll = 0;
            false
        }
        _ => false,
    }
}

fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).split(frame.area());

    // Scrollable, read-only log view:
    let log_area = chunks[0];
    let inner_height = log_area.height.saturating_sub(2) as usize; // borders
    let max_scroll = app.lines.len().saturating_sub(inner_height.max(1));
    let scroll_offset = max_scroll.saturating_sub(app.scroll as usize) as u16;
    let logs = Paragraph::new(Text::from(app.lines.as_slice()))
        .block(Block::default().borders(Borders::ALL).title("Logs"))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset, 0));
    frame.render_widget(logs, log_area);

    // Command input field:
    let input_area = chunks[1];
    let input = Paragraph::new(format!("> {}", app.input)).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Command (start|stop|restart <service>, status, shutdown, quit)"),
    );
    frame.render_widget(input, input_area);

    let inner_width = input_area.width.saturating_sub(2); // borders
    let cursor_x = input_area.x
        + 1 // left border
        + 2 // "> " prefix
        + (app.input[..app.cursor].chars().count() as u16).min(inner_width.saturating_sub(1));
    frame.set_cursor_position((cursor_x, input_area.y + 1));
}
