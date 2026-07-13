use std::panic;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SendError, Sender, TryRecvError};
use std::thread::{self, JoinHandle, sleep};
use std::time::Duration;

use crate::config::{Args, Command, Config, ServiceRestartPolicy, ServiceState};
use crate::error::{ProcessError, WorkerError};
use crate::process::{Process, ProcessState};
use crate::resolver;

#[derive(Debug)]
pub enum WorkerMessage {
    AutostartServices,
    StartService(String),
    StopService(String),
    PrintStatus,
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

fn process_message(message: WorkerMessage, state: &mut WorkerState) -> Result<(), WorkerError> {
    match message {
        WorkerMessage::AutostartServices => start_services(state),
        WorkerMessage::StartService(service) => start_service(state, service),
        WorkerMessage::StopService(service) => stop_service(state, service),
        WorkerMessage::PrintStatus => {
            print_status(state);
            Ok(())
        }
    }
}

fn start_services(state: &mut WorkerState) -> Result<(), WorkerError> {
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

    log::info!("Resolving dependencies...");
    let start_names = services
        .iter()
        .filter(|service| {
            matches!(
                service.state.clone().unwrap_or_default(),
                ServiceState::Enabled
            )
        })
        .map(|service| service.name.clone())
        .collect();
    let topological_order = match resolver::resolve_dependencies(start_names, services) {
        Ok(order) => order,
        Err(e) => {
            log::error!(" >>> couldn't resolve: {e}");
            return Err(e.into());
        }
    };
    log::info!(
        " >>> resolved: {}",
        topological_order
            .iter()
            .map(|service| service.name.clone())
            .reduce(|acc, s| format!("{acc}, {s}"))
            .unwrap()
    );

    log::info!("Starting services...");
    log::info!("Press Ctrl+C to kill all processes");

    for service in topological_order.iter() {
        match service.state.clone().unwrap_or_default() {
            ServiceState::Enabled | ServiceState::Disabled => {
                // disabled services might come from resolved dependencies. start anyways
            }
            ServiceState::Masked => {
                // masked services should never start
                log::warn!(" >>> {}: masked", service.name);
                continue;
            }
        }

        if let Some(ref cond) = service.cond {
            match run_condition(&service.name, cond, log_path, config) {
                Ok(status) => {
                    if !status.success() {
                        log::warn!(
                            " >>> {}: condition returned non-zero exit code ({})",
                            service.name,
                            status
                        );
                        continue;
                    }
                }
                Err(error) => {
                    log::error!(" >>> {}: condition failed to run: {}", service.name, error);
                    continue;
                }
            }
        }

        log::info!(" >>> {}: starting", service.name);
        match Process::new(service).log_path(log_path).spawn(config) {
            Ok(process) => processes.push(process),
            Err(error) => {
                log::error!(" >>> {} failed to start because {:?}", service.name, error);
                if service.required.unwrap_or(false) {
                    log::error!(" >>> required process '{}' didn't start", service.name);
                    return Err(WorkerError::RequiredProcessGone);
                }
            }
        };

        if let Some(milliseconds) = service.wait.or(config.wait) {
            sleep(Duration::from_millis(milliseconds));
        }
        if stopped.load(Ordering::Relaxed) {
            return Err(WorkerError::ManuallyStopped);
        }
    }

    log::info!("Monitoring services...");
    Ok(())
}

fn monitor_processes(state: &mut WorkerState) -> Result<(), WorkerError> {
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
                            return Err(WorkerError::RequiredProcessGone);
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
                        return Err(WorkerError::RequiredProcessGone);
                    }
                }
            }
        }
    }
    if alive == 0 {
        return Err(WorkerError::AllServicesStopped);
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
        .unchecked()
        .run()?;

    Ok(output.status)
}

pub fn run_condition(
    service_name: &str,
    condition: &Command,
    log_path: &str,
    config: &Config,
) -> Result<ExitStatus, ProcessError> {
    let cmd = if let Some(ref vars) = config.vars
        && !config.disable_var_substitution
    {
        condition.parse_with_subst(vars)
    } else {
        condition.parse()
    }
    .ok_or(ProcessError::CommandParse(service_name.to_string()))?;

    let program = cmd[0].as_str();
    let args = &cmd[1..];

    let log_file = PathBuf::from(log_path).join(format!("log-{}-condition.txt", service_name));

    let output = duct::cmd(program, args)
        .stderr_to_stdout()
        .stdout_path(log_file)
        .unchecked()
        .run()?;

    Ok(output.status)
}

fn start_service(state: &mut WorkerState, service_name: String) -> Result<(), WorkerError> {
    let processes = &mut state.processes;
    let config = &state.config;
    let services = &config.services;
    let log_path = &state.args.logs;

    log::info!("Starting service '{service_name}'...");
    log::info!(" >>> Resolving dependencies...");
    let topological_order =
        match resolver::resolve_dependencies(vec![service_name.clone()], services) {
            Ok(order) => order,
            Err(e) => {
                log::error!(" >>> couldn't resolve: {e}");
                return Ok(()); // Do not crash
            }
        };
    log::info!(
        " >>> resolved: {}",
        topological_order
            .iter()
            .map(|service| service.name.clone())
            .reduce(|acc, s| format!("{acc}, {s}"))
            .unwrap()
    );

    for service in topological_order {
        // Skip masked services:
        match service.state.clone().unwrap_or_default() {
            ServiceState::Enabled | ServiceState::Disabled => {}
            ServiceState::Masked => {
                log::error!(" >>> {}: masked, won't start", service.name);
                continue;
            }
        }

        // Check if service is already running, or needs to be restarted:
        let mut found = false;
        for process in processes.iter_mut() {
            if process.service.name == service.name {
                if let ProcessState::Running(_) = process.state {
                    log::info!(" >>> {}: already running", service.name);
                } else {
                    match process.spawn_mut(config) {
                        Ok(_) => {
                            log::info!(" >>> {}: restarted", service.name);
                            if let Some(milliseconds) = service.wait.or(config.wait) {
                                sleep(Duration::from_millis(milliseconds));
                            }
                        }
                        Err(error) => {
                            log::error!(
                                " >>> {}: failed to restart because {:?}",
                                service.name,
                                error
                            );
                        }
                    }
                }
                found = true;
                break;
            }
        }
        if found {
            continue;
        }

        // If service not in process list, create new process:
        match Process::new(&service).log_path(log_path).spawn(config) {
            Ok(process) => {
                log::info!(" >>> {}: started", service.name);
                processes.push(process);
                if let Some(milliseconds) = service.wait.or(config.wait) {
                    sleep(Duration::from_millis(milliseconds));
                }
            }
            Err(error) => {
                log::error!(" >>> {}: failed to start because {:?}", service.name, error);
            }
        };
    }

    Ok(())
}

fn stop_service(state: &mut WorkerState, service_name: String) -> Result<(), WorkerError> {
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

fn print_status(state: &mut WorkerState) {
    let mut running = 0;
    let mut exited = 0;
    log::info!("Current status:");
    for service in state.config.services.iter() {
        let mut process_state;
        match service.state {
            Some(ServiceState::Enabled) | None => process_state = "not running",
            Some(ServiceState::Disabled) => process_state = "not running (disabled)",
            Some(ServiceState::Masked) => process_state = "not running (masked)",
        }
        for process in state.processes.iter() {
            if service.name == process.service.name {
                match process.state {
                    ProcessState::Running(_) => {
                        if process.restarted {
                            process_state = "running (was restarted)";
                        } else {
                            process_state = "running";
                        }
                        running += 1;
                    }
                    ProcessState::Exited => {
                        process_state = "exited";
                        exited += 1;
                    }
                    _ => {}
                }
            }
        }
        log::info!(" >>> {}: {}", service.name, process_state);
    }
    log::info!(
        " >>> {} running, {} exited, {} total",
        running,
        exited,
        state.config.services.len()
    );
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
