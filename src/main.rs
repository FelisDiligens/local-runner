mod config;
mod error;
mod process;
mod utils;

#[cfg(test)]
mod tests;

use std::{
    env, fs,
    process::ExitCode,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use clap::Parser;
use clone_macro::clone;

use crate::config::Args;
use crate::config::load_config;
use crate::error::{ConfigError, ProcessError};
use crate::process::{kill_processes, monitor_processes, start_services, Process};

fn register_ctrlc_handler() -> Arc<AtomicBool> {
    // Handle Ctrl+C by storing it in a shared boolean:
    let ctrl_c_pressed = Arc::new(AtomicBool::new(false));
    ctrlc::set_handler(clone!([ctrl_c_pressed], move || {
        #[cfg(windows)]
        println!("^C -- Keyboard interrupt received");
        #[cfg(not(windows))]
        println!(" -- Keyboard interrupt received");
        ctrl_c_pressed.store(true, Ordering::Relaxed);
    }))
    .expect("Error setting Ctrl+C handler");

    ctrl_c_pressed
}

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
    let mut processes: Vec<Process> = Vec::with_capacity(config.services.len());

    match start_services(&mut processes, &config, &args.logs, ctrl_c_pressed.clone())
        .and_then(|_| monitor_processes(&mut processes, &config, ctrl_c_pressed.clone()))
    {
        Ok(_) => ExitCode::SUCCESS,
        Err(ProcessError::CtrlC) => {
            kill_processes(&mut processes, &config, &args.logs).unwrap();
            ExitCode::SUCCESS
        }
        Err(ProcessError::RequiredProcessGone) => {
            kill_processes(&mut processes, &config, &args.logs).unwrap();
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("ERROR: Unexpected error.");
            panic!("{:?}", error)
        }
    }
}
