mod config;
mod error;
mod ipc;
mod process;
mod resolver;
mod utils;
mod worker;

#[cfg(test)]
mod tests;

use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;
use std::{env, fs, process::ExitCode};

use clap::Parser;

use crate::config::CommandArgs;
use crate::utils::{register_ctrlc_handler, setup_stdout_logger};
use crate::{config::Args, worker::Worker};
use crate::{config::load_config, error::WorkerError};
use crate::{error::ConfigError, worker::WorkerMessage};

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
        CommandArgs::Start { service } => format!("start {service}"),
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

fn main() -> ExitCode {
    setup_stdout_logger().unwrap();
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

    let ctrl_c_pressed = register_ctrlc_handler();
    let mut ipc_errored = false;

    let mut worker = Worker::spawn(config, args.clone());
    worker.queue(WorkerMessage::AutostartServices).unwrap();
    while !worker.is_finished() && !worker.is_stopped() {
        if ctrl_c_pressed.load(Ordering::Relaxed) {
            worker.stop();
        }
        match ipc::read_message(&args.path) {
            Ok(Some(message)) => {
                let mut splits = message.split(" ");
                if let Some(command) = splits.next()
                    && let Some(service) = splits.next()
                {
                    match command {
                        "restart" => {
                            log::trace!("IPC: Restarting service {service} scheduled");
                            worker
                                .queue(WorkerMessage::StopService(service.to_string()))
                                .unwrap();
                            worker
                                .queue(WorkerMessage::StartService(service.to_string()))
                                .unwrap();
                        }
                        "start" => {
                            log::trace!("IPC: Starting service {service} scheduled");
                            worker
                                .queue(WorkerMessage::StartService(service.to_string()))
                                .unwrap();
                        }
                        "stop" => {
                            log::trace!("IPC: Stopping service {service} scheduled");
                            worker
                                .queue(WorkerMessage::StopService(service.to_string()))
                                .unwrap();
                        }
                        "status" => {
                            log::trace!("IPC: Printing status scheduled");
                            worker.queue(WorkerMessage::PrintStatus).unwrap();
                        }
                        "shutdown" => {
                            log::trace!("IPC: Shutting down scheduled");
                            worker.stop()
                        }
                        _ => {
                            log::error!("Unknown IPC message: {command}");
                        }
                    }
                    // Truncate IPC file after processing it's message:
                    ipc::write_message("", &args.path).unwrap();
                }
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
        Err(error) => {
            log::error!("Unexpected error.");
            panic!("{:?}", error)
        }
    }
}
