use std::io::{self, Stdout};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
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

struct App {
    input: String,
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
}

impl App {
    fn new() -> Self {
        Self {
            input: String::new(),
            scroll: 0,
            lines: Vec::new(),
            history: Vec::new(),
            history_index: None,
            stashed_input: String::new(),
        }
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

/// Runs the TUI on the current (main) thread until the worker stops or finishes.
/// Returns Err only if the terminal couldn't be initialized.
pub fn run(
    worker: &Worker,
    args: &Args,
    ctrl_c_pressed: &Arc<AtomicBool>,
    log_buffer: &LogBuffer,
    log_control: &LogControl,
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
    let result = run_loop(&mut terminal, worker, args, ctrl_c_pressed, log_buffer);
    log_control.set_tui_active(false);
    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    worker: &Worker,
    args: &Args,
    ctrl_c_pressed: &Arc<AtomicBool>,
    log_buffer: &LogBuffer,
) -> io::Result<()> {
    let mut app = App::new();
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
    match key.code {
        // In raw mode Ctrl+C arrives as a key event instead of SIGINT:
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            worker.stop();
            true
        }
        KeyCode::Enter => {
            let command = app.input.trim().to_string();
            app.input.clear();
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
        KeyCode::Backspace => {
            app.input.pop();
            false
        }
        KeyCode::Char(c) => {
            app.input.push(c);
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
        + (app.input.chars().count() as u16).min(inner_width.saturating_sub(1));
    frame.set_cursor_position((cursor_x, input_area.y + 1));
}
