use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use colored::Colorize;
use log::{Level, Log, Metadata, Record};

/// Maximum number of log lines kept in the buffer for the TUI.
const MAX_BUFFERED_LINES: usize = 10_000;

/// Third-party log targets that are too noisy (e.g. the mio poller registration
/// trace logs emitted by crossterm's event source).
const FILTERED_TARGETS: &[&str] = &["mio"];

/// A thread-safe, bounded buffer of log lines shared between the logger and the TUI.
#[derive(Clone, Default)]
pub struct LogBuffer {
    lines: Arc<Mutex<VecDeque<(Level, String)>>>,
}

impl LogBuffer {
    fn push(&self, level: Level, message: String) {
        let mut lines = self.lines.lock().unwrap();
        if lines.len() >= MAX_BUFFERED_LINES {
            lines.pop_front();
        }
        lines.push_back((level, message));
    }

    /// Removes and returns all buffered lines.
    pub fn drain(&self) -> Vec<(Level, String)> {
        self.lines.lock().unwrap().drain(..).collect()
    }
}

/// Shared control between main/TUI and the logger. While the TUI is active,
/// stdout output is suppressed so it doesn't corrupt the TUI display.
#[derive(Clone, Default)]
pub struct LogControl {
    inner: Arc<LogControlInner>,
}

#[derive(Default)]
struct LogControlInner {
    tui_active: AtomicBool,
    /// Held while checking `tui_active` and writing to stdout, so that flipping
    /// the flag in `set_tui_active` cannot race with an in-flight stdout write
    /// (a write landing after raw mode was enabled would garble the screen,
    /// because in raw mode `\n` doesn't carriage-return).
    stdout_guard: Mutex<()>,
}

impl LogControl {
    /// Sets whether the TUI is active. Call this before entering raw mode.
    pub fn set_tui_active(&self, active: bool) {
        let _guard = self.inner.stdout_guard.lock().unwrap();
        self.inner.tui_active.store(active, Ordering::Relaxed);
    }

    pub fn is_tui_active(&self) -> bool {
        self.inner.tui_active.load(Ordering::Relaxed)
    }

    fn write_stdout(&self, level: Level, message: &str) {
        let _guard = self.inner.stdout_guard.lock().unwrap();
        if self.inner.tui_active.load(Ordering::Relaxed) {
            return;
        }
        let colored = match level {
            Level::Error => message.red(),
            Level::Warn => message.yellow(),
            Level::Info => message.into(),
            Level::Debug | Level::Trace => message.white(),
        };
        let mut stdout = std::io::stdout().lock();
        let _ = writeln!(stdout, "{colored}");
    }
}

/// Logger that writes to stdout (like the previous fern setup) and additionally
/// stores every line in a shared buffer for the TUI. While the TUI is active,
/// stdout output is suppressed so it doesn't corrupt the TUI display.
struct TeeLogger {
    buffer: LogBuffer,
    control: LogControl,
}

impl Log for TeeLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        !is_filtered(metadata.target())
    }

    fn log(&self, record: &Record) {
        // The log macros call log() without consulting enabled(), so filter here:
        if is_filtered(record.target()) {
            return;
        }
        let message = record.args().to_string();
        self.buffer.push(record.level(), message.clone());
        self.control.write_stdout(record.level(), &message);
    }

    fn flush(&self) {}
}

fn is_filtered(target: &str) -> bool {
    FILTERED_TARGETS
        .iter()
        .any(|filtered| target.starts_with(filtered))
}

pub fn setup(buffer: &LogBuffer, control: &LogControl) -> Result<(), log::SetLoggerError> {
    log::set_boxed_logger(Box::new(TeeLogger {
        buffer: buffer.clone(),
        control: control.clone(),
    }))?;
    log::set_max_level(log::LevelFilter::Trace);
    Ok(())
}
