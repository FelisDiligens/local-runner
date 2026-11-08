mod config;
mod error;
mod ipc;
mod logger;
mod process;
mod resolver;
mod tui;
mod utils;
mod worker;

#[cfg(test)]
mod tests;

use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::sleep;
use std::time::Duration;
use std::{env, fs, process::ExitCode};

use clap::Parser;

use crate::config::CommandArgs;
use crate::logger::{LogBuffer, LogControl};
use crate::utils::register_ctrlc_handler;
use crate::{config::Args, worker::Worker};
use crate::{config::load_config, error::WorkerError};
use crate::{error::ConfigError, worker::WorkerMessage};

/// Parses a command string like "start my-service", "start @my-tag", "stop my-service",
/// "restart my-service", "status" or "shutdown" and queues the corresponding worker messages.
/// Used by both the IPC file and the TUI command field.
fn dispatch_command(message: &str, worker: &Worker) {
    let mut splits = message.split_whitespace();
    let Some(command) = splits.next() else {
        return;
    };
    let service = splits.next();
    match command {
        "restart" => {
            if let Some(service) = service {
                log::trace!("Restarting service {service} scheduled");
                worker
                    .queue(WorkerMessage::StopService(service.to_string()))
                    .unwrap();
                worker
                    .queue(WorkerMessage::StartService(service.to_string()))
                    .unwrap();
            }
        }
        "start" => {
            if let Some(service) = service {
                if service.starts_with('@') {
                    let tag_name = service.trim_start_matches('@');
                    log::trace!("Starting tag {tag_name} scheduled");
                    worker
                        .queue(WorkerMessage::StartTag(tag_name.to_string()))
                        .unwrap();
                } else {
                    log::trace!("Starting service {service} scheduled");
                    worker
                        .queue(WorkerMessage::StartService(service.to_string()))
                        .unwrap();
                }
            }
        }
        "stop" => {
            if let Some(service) = service {
                log::trace!("Stopping service {service} scheduled");
                worker
                    .queue(WorkerMessage::StopService(service.to_string()))
                    .unwrap();
            }
        }
        "status" => {
            log::trace!("Printing status scheduled");
            worker.queue(WorkerMessage::PrintStatus).unwrap();
        }
        "shutdown" => {
            log::trace!("Shutting down scheduled");
            worker.stop();
        }
        _ => {
            log::error!("Unknown command: {command}");
        }
    }
}

fn process_command(args: &Args, command: &CommandArgs) -> ExitCode {
    if !ipc::get_ipc_file(&args.path).exists() {
        log::error!("Daemon is not running.");
        log::info!(
            "You need to start local-runner before running start|stop|restart commands have any effect."
        );
        return ExitCode::FAILURE;
    }
    let message = match command {
        CommandArgs::Restart { service } => format!("restart {service}"),
        CommandArgs::Start {
            tag,
            service_or_tag,
        } => {
            if *tag {
                format!("start @{service_or_tag}")
            } else {
                format!("start {service_or_tag}")
            }
        }
        CommandArgs::Stop { service } => format!("stop {service}"),
        CommandArgs::Status => "status _".to_string(),
        CommandArgs::Shutdown => "shutdown _".to_string(),
    };
    match ipc::write_message(message, &args.path) {
        Ok(_) => {
            log::info!("Message written. Check output of the already running instance.");
            ExitCode::SUCCESS
        }
        Err(e) => {
            log::error!("Couldn't write ipc file: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The original non-TUI loop: poll the IPC file and the Ctrl+C flag once per second.
fn run_plain_loop(worker: &Worker, args: &Args, ctrl_c_pressed: &Arc<AtomicBool>) {
    let mut ipc_errored = false;
    while !worker.is_finished() && !worker.is_stopped() {
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
        sleep(Duration::from_millis(1000));
    }
}

fn main() -> ExitCode {
    let log_buffer = LogBuffer::default();
    let log_control = LogControl::default();
    logger::setup(&log_buffer, &log_control).unwrap();
    let args = Args::parse();
    if let Some(ref command) = args.command {
        return process_command(&args, command);
    }
    let config = match load_config(&args.path) {
        Ok(config) => config,
        Err(ConfigError::IOError(error)) => {
            log::error!(" >>> Couldn't find/read {}: {}\n", args.path, error);
            log::info!("You may want to specify the path to a config file:");
            log::info!("$ local-runner --path <PATH>");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            log::error!(" >>> Couldn't load {}: {}", args.path, error);
            return ExitCode::FAILURE;
        }
    };

    if let Some(ref env) = config.env {
        log::info!("Setting global environment variables:");
        for (key, val) in env {
            let val = if config.disable_env_interpolation {
                val.to_string()
            } else {
                utils::expand_env_vars(val)
            };
            log::info!(" >>> {key}={val}");
            unsafe {
                env::set_var(key, val);
            };
        }
    }

    log::info!("Writing log files to {}", args.logs);
    if fs::create_dir(&args.logs).is_ok() {
        log::info!(" >>> Created directory {}", args.logs);
    }

    if let Err(e) = ipc::write_message("", &args.path) {
        log::error!("Couldn't write IPC file: {e}");
        log::warn!(" >>> start|stop|restart commands will have no effect")
    }

    let ctrl_c_pressed = register_ctrlc_handler(&log_control);

    let mut worker = Worker::spawn(config, args.clone());
    worker.queue(WorkerMessage::AutostartServices).unwrap();

    let use_tui = !args.no_tui && std::io::stdout().is_terminal();
    let mut tui_ran = false;
    if use_tui {
        match tui::run(&worker, &args, &ctrl_c_pressed, &log_buffer, &log_control) {
            Ok(()) => tui_ran = true,
            Err(e) => log::error!("Couldn't start TUI: {e}. Falling back to plain output."),
        }
    }
    if !tui_ran {
        run_plain_loop(&worker, &args, &ctrl_c_pressed);
    }
    let _ = ipc::delete_ipc_file(&args.path);
    match worker.join() {
        Ok(_) | Err(WorkerError::ManuallyStopped) | Err(WorkerError::AllServicesStopped) => {
            log::info!("All services stopped. Good bye!");
            ExitCode::SUCCESS
        }
        Err(WorkerError::RequiredProcessGone) => {
            log::info!("All services stopped. Required process was gone.");
            ExitCode::FAILURE
        }
        Err(
            WorkerError::CyclicDepedencies
            | WorkerError::MaskedDepedencies
            | WorkerError::UnknownDependency(_),
        ) => {
            log::info!("All services stopped. Couldn't resolve all dependencies.");
            ExitCode::FAILURE
        }
        Err(error) => {
            log::error!("Unexpected error.");
            panic!("{:?}", error)
        }
    }
}
