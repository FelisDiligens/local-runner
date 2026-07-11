use std::panic;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SendError, Sender, TryRecvError};
use std::thread::{self, JoinHandle, sleep};
use std::time::Duration;

use crate::config::{Args, Command, Config, ServiceRestartPolicy};
use crate::error::{ProcessError, WorkerError};
use crate::process::{Process, ProcessState};

#[derive(Debug)]
pub enum WorkerMessage {
    AutostartServices,
    StartService(String),
    StopService(String),
}

pub type WorkerResult<T> = Result<T, WorkerError>;

pub struct Worker {
    sender: Sender<WorkerMessage>,
    handle: Option<JoinHandle<WorkerResult<()>>>,
    stopped: Arc<AtomicBool>,
}

impl Worker {
    pub fn spawn(config: Config, args: Args) -> Self {
        let (sender, receiver) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = thread::spawn({
            let stopped = stopped.clone();
            move || run_loop(receiver, config, args, stopped)
        });
        Self {
            sender,
            handle: Some(handle),
            stopped,
        }
    }

    pub fn queue(&self, msg: WorkerMessage) -> Result<(), SendError<WorkerMessage>> {
        self.sender.send(msg)
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    pub fn join(&mut self) -> WorkerResult<()> {
        if let Some(handle) = self.handle.take() {
            match handle.join() {
                Ok(value) => value,
                Err(e) => {
                    log::error!("Unexpected error, worker thread died.");
                    panic::resume_unwind(e)
                }
            }
        } else {
            Ok(())
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    pub fn is_finished(&self) -> bool {
        if let Some(handle) = self.handle.as_ref() {
            return handle.is_finished();
        }
        true
    }
}

struct WorkerState {
    processes: Vec<Process>,
    config: Config,
    args: Args,
    stopped: Arc<AtomicBool>,
}

fn run_loop(
    receiver: Receiver<WorkerMessage>,
    config: Config,
    args: Args,
    stopped: Arc<AtomicBool>,
) -> WorkerResult<()> {
    let mut state = WorkerState {
        processes: Vec::with_capacity(config.services.len()),
        config,
        args,
        stopped,
    };
    loop {
        match receiver.try_recv() {
            Ok(message) => {
                if let Err(e) = process_message(message, &mut state) {
                    kill_processes(&mut state)?;
                    return Err(e.into());
                }
            }
            Err(TryRecvError::Disconnected) => {
                kill_processes(&mut state)?;
                return Err(WorkerError::Disconnected);
            }
            _ => {}
        }
        if let Err(e) = monitor_processes(&mut state) {
            kill_processes(&mut state)?;
            return Err(e.into());
        }
        if state.stopped.load(Ordering::Relaxed) {
            kill_processes(&mut state)?;
            return Err(WorkerError::ManuallyStopped);
        }
    }
}

fn process_message(message: WorkerMessage, state: &mut WorkerState) -> Result<(), ProcessError> {
    match message {
        WorkerMessage::AutostartServices => start_services(state),
        WorkerMessage::StartService(service) => start_service(state, service),
        WorkerMessage::StopService(service) => stop_service(state, service),
    }
}

fn start_services(state: &mut WorkerState) -> Result<(), ProcessError> {
    let config = &state.config;
    let processes = &mut state.processes;
    let services = &config.services;
    let hooks = config.hooks.as_ref();
    let log_path = &state.args.logs;
    let stopped = &state.stopped;

    // Run prepare hook:
    if let Some(hook) = hooks.and_then(|hooks| hooks.prepare.as_ref()) {
        match run_hook("prepare", hook, log_path, config) {
            Ok(status) => log::info!(" >>> Hook exited ({})", status),
            Err(error) => log::error!(" >>> Hook failed! {}", error),
        }
    }

    log::info!("Starting services...");
    log::info!("Press Ctrl+C to kill all processes");

    for service in services {
        if !service.enabled.unwrap_or(true) {
            log::info!(" >>> {}: skipped (disabled)", service.name);
            continue;
        }
        log::info!(" >>> {}: starting", service.name);
        match Process::new(service).log_path(log_path).spawn(config) {
            Ok(process) => processes.push(process),
            Err(error) => {
                log::error!(" >>> {} failed to start because {:?}", service.name, error);
                if service.required.unwrap_or(false) {
                    log::error!(" >>> required process '{}' didn't start", service.name);
                    return Err(ProcessError::RequiredProcessGone);
                }
            }
        };

        if let Some(milliseconds) = service.wait.or(config.wait) {
            sleep(Duration::from_millis(milliseconds));
        }
        if stopped.load(Ordering::Relaxed) {
            return Err(ProcessError::ManuallyStopped);
        }
    }

    log::info!("Monitoring services...");
    Ok(())
}

fn monitor_processes(state: &mut WorkerState) -> Result<(), ProcessError> {
    let config = &state.config;
    let processes = &mut state.processes;

    // Keep track of processes that are still running.
    let mut alive = 0;

    // Check each still running process:
    for process in processes {
        let ProcessState::Running(ref handle) = process.state else {
            continue;
        };

        match handle.try_wait()? {
            None => {
                alive += 1;
            }
            Some(output) => {
                let status = output.status;
                process.state = ProcessState::Exited;

                let state = if status.success() {
                    "exited"
                } else {
                    "crashed"
                };
                log::info!(" >>> {}: {} ({})", process.service.name, state, status);

                let is_required = process.service.required.unwrap_or(false);
                let has_crashed = !status.success();
                let restart_policy = process.service.restart.clone().unwrap_or_default();
                let has_restarted_previously = process.restarted;

                let should_restart = !has_restarted_previously
                    && (restart_policy == ServiceRestartPolicy::Always
                        || (restart_policy == ServiceRestartPolicy::OnFailure
                            && (is_required || has_crashed)));

                if should_restart {
                    log::info!(" >>> Restarting {}", process.service.name);
                    process.restarted = true;
                    if let Err(error) = process.spawn_mut(config) {
                        log::error!(
                            " >>> {} failed to restart because {:?}",
                            process.service.name,
                            error
                        );
                        if is_required {
                            log::error!(
                                " >>> required process '{}' didn't restart",
                                process.service.name
                            );
                            return Err(ProcessError::RequiredProcessGone);
                        }
                    };
                } else {
                    if has_restarted_previously {
                        log::warn!(
                            " >>> Process '{}' has crashed twice, not restarting",
                            process.service.name
                        );
                    }
                    if is_required {
                        log::error!(
                            " >>> required process '{}' {}...",
                            process.service.name,
                            state
                        );
                        return Err(ProcessError::RequiredProcessGone);
                    }
                }
            }
        }
    }
    if alive == 0 {
        return Err(ProcessError::AllServicesStopped);
    }
    Ok(())
}

fn run_hook(
    hook_name: &str,
    hook: &Command,
    log_path: &str,
    config: &Config,
) -> Result<ExitStatus, ProcessError> {
    let cmd = if let Some(ref vars) = config.vars
        && !config.disable_var_substitution
    {
        hook.parse_with_subst(vars)
    } else {
        hook.parse()
    }
    .ok_or(ProcessError::CommandParse(hook_name.to_string()))?;

    log::info!(
        "Running '{}' hook: {}",
        hook_name,
        shlex::try_join(cmd.iter().map(|s| s.as_str())).unwrap_or(cmd.join(" ")),
    );

    let program = cmd[0].as_str();
    let args = &cmd[1..];

    let log_file = PathBuf::from(log_path).join(format!("log-{}-hook.txt", hook_name));

    let output = duct::cmd(program, args)
        .stderr_to_stdout()
        .stdout_path(log_file)
        .run()?;

    Ok(output.status)
}

fn start_service(state: &mut WorkerState, service_name: String) -> Result<(), ProcessError> {
    let processes = &mut state.processes;
    let config = &state.config;
    let services = &config.services;
    let log_path = &state.args.logs;

    log::info!("Starting service '{service_name}'...");
    for process in processes.iter_mut() {
        if process.service.name == service_name {
            if let ProcessState::Running(_) = process.state {
                log::info!(" >>> Process already running");
            } else {
                if let Err(error) = process.spawn_mut(config) {
                    log::error!(" >>> Process failed to restart because {error:?}",);
                };
            }
            return Ok(());
        }
    }

    for service in services {
        if service.name == service_name {
            match Process::new(service).log_path(log_path).spawn(config) {
                Ok(process) => processes.push(process),
                Err(error) => {
                    log::error!(" >>> Process failed to start because {:?}", error);
                }
            };
            return Ok(());
        }
    }

    log::error!(" >>> Service not found in config file");
    Ok(())
}

fn stop_service(state: &mut WorkerState, service_name: String) -> Result<(), ProcessError> {
    let processes = &mut state.processes;
    let config = &state.config;

    log::info!("Stopping service '{service_name}'...");
    for process in processes {
        if process.service.name == service_name {
            if let ProcessState::Running(ref handle) = process.state {
                let is_required = process.service.required.unwrap_or(false);
                match process.kill(config.use_taskkill) {
                    Ok(_) => {
                        let output = handle.wait()?;
                        log::info!(" >>> killed ({})", output.status);
                    }
                    Err(error) => {
                        log::error!(" >>> couldn't kill, error: {}", error)
                    }
                }
                if is_required {
                    log::warn!(" >>> this process was marked as required!");
                }
            } else {
                log::info!(" >>> Service was not running");
                return Ok(());
            }
            process.state = ProcessState::Exited;
            return Ok(());
        }
    }

    log::error!(" >>> Service not found in process list");
    Ok(())
}

fn kill_processes(state: &mut WorkerState) -> Result<(), ProcessError> {
    let processes = &mut state.processes;
    let config = &state.config;
    let log_path = &state.args.logs;

    log::info!("Killing services...");
    for process in processes {
        if let ProcessState::Running(ref handle) = process.state {
            match process.kill(config.use_taskkill) {
                Ok(_) => {
                    let output = handle.wait()?;
                    log::info!(" >>> {}: killed ({})", process.service.name, output.status);
                }
                Err(error) => log::error!(
                    " >>> {}: couldn't kill, error: {}",
                    process.service.name,
                    error
                ),
            }
        } else {
            continue;
        }
        process.state = ProcessState::Exited;
    }
    // Run cleanup hook:
    if let Some(Some(cleanup_hook)) = config.hooks.as_ref().map(|hooks| hooks.cleanup.as_ref()) {
        match run_hook("cleanup", cleanup_hook, log_path, config) {
            Ok(status) => log::info!(" >>> Hook exited ({})", status),
            Err(error) => log::error!(" >>> Hook failed! {}", error),
        }
    }
    Ok(())
}
