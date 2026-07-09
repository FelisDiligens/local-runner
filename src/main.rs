mod config;
mod error;
mod process;
mod utils;
mod worker;

#[cfg(test)]
mod tests;

use std::sync::atomic::Ordering;
use std::thread::sleep;
use std::time::Duration;
use std::{env, fs, process::ExitCode};

use clap::Parser;

use crate::utils::register_ctrlc_handler;
use crate::{config::Args, worker::Worker};
use crate::{config::load_config, error::WorkerError};
use crate::{error::ConfigError, worker::WorkerMessage};

fn main() -> ExitCode {
    let args = Args::parse();
    let config = match load_config(&args.path) {
        Ok(config) => config,
        Err(ConfigError::IOError(error)) => {
            eprintln!(" >>> ERROR: Couldn't find/read {}: {}\n", args.path, error);
            println!("You may want to specify the path to a config file:");
            println!("$ local-runner --path <PATH>");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!(" >>> ERROR: Couldn't load {}: {}", args.path, error);
            return ExitCode::FAILURE;
        }
    };

    if let Some(ref env) = config.env {
        println!("Setting global environment variables:");
        for (key, val) in env {
            let val = if config.disable_env_interpolation {
                val.to_string()
            } else {
                utils::expand_env_vars(val)
            };
            eprintln!(" >>> {key}={val}");
            unsafe {
                env::set_var(key, val);
            };
        }
    }

    println!("Writing log files to {}", args.logs);
    if fs::create_dir(&args.logs).is_ok() {
        println!(" >>> Created directory {}", args.logs);
    }

    let ctrl_c_pressed = register_ctrlc_handler();

    let mut worker = Worker::spawn(config, args);
    worker.queue(WorkerMessage::AutostartServices).unwrap();
    while !worker.is_finished() && !worker.is_stopped() {
        if ctrl_c_pressed.load(Ordering::Relaxed) {
            worker.stop();
        }
        sleep(Duration::from_millis(1000));
    }
    match worker.join() {
        Ok(_) | Err(WorkerError::ManuallyStopped) | Err(WorkerError::AllServicesStopped) => {
            println!("All services stopped. Good bye!");
            ExitCode::SUCCESS
        }
        Err(WorkerError::RequiredProcessGone) => {
            println!("All services stopped. Required process was gone.");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("ERROR: Unexpected error.");
            panic!("{:?}", error)
        }
    }
}
