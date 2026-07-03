use std::{
    path::PathBuf,
    process::ExitStatus,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::sleep,
    time::Duration,
};

use duct::Handle;

use crate::{
    config::{Command, Config, Service, ServiceRestartPolicy},
    error::ProcessError,
    utils,
};

#[derive(Default)]
pub enum ProcessState {
    Running(Box<Handle>),
    Exited,
    #[default]
    None,
}

pub struct Process {
    pub service: Service,
    pub state: ProcessState,
    pub restarted: bool,
    log_path: Option<String>,
}

impl Process {
    pub fn new(service: &Service) -> Self {
        Self {
            service: service.clone(),
            state: ProcessState::None,
            restarted: false,
            log_path: None,
        }
    }

    pub fn log_path(mut self, log_path: &str) -> Self {
        self.log_path = Some(log_path.to_string());
        self
    }

    pub fn spawn_mut(&mut self, config: &Config) -> Result<(), ProcessError> {
        // Parse command:
        let cmd = if let Some(ref vars) = config.vars
            && !config.disable_var_substitution
        {
            self.service.cmd.parse_with_subst(vars)
        } else {
            self.service.cmd.parse()
        }
        .ok_or(ProcessError::CommandParse(self.service.name.clone()))?;

        // Create expression from program and cli args:
        let program = cmd[0].as_str();
        let args = &cmd[1..];
        let mut expr = duct::cmd(program, args);

        // Pass environment variables to expression:
        if let Some(ref env) = self.service.env {
            for (name, val) in env {
                if config.disable_env_interpolation {
                    expr = expr.env(name, val);
                } else {
                    expr = expr.env(name, utils::expand_env_vars(val));
                }
            }
        }

        let create_window = self.service.create_window.unwrap_or(false);
        if cfg!(not(windows)) && create_window {
            println!(
                " >>> WARNING: create_window doesn't have an effect on platforms other than Windows!"
            );
        }

        if cfg!(windows) && create_window {
            println!(
                " >>> INFO: No log file will be written for '{}' (create_window = true)",
                self.service.name
            );

            expr = expr.before_spawn(utils::create_new_console);
        } else {
            // Redirect stdout/stderr to log file, if path is given:
            if let Some(log_path) = self.log_path.as_ref() {
                let log_file =
                    PathBuf::from(log_path).join(format!("log-{}.txt", self.service.name));
                expr = expr.stderr_to_stdout().stdout_path(log_file);
            }

            // Create new process group, so SIGINT is not propagated:
            expr = expr.before_spawn(utils::create_new_process_group);
        }

        // Set current working directory, if given:
        if let Some(pwd) = self.service.pwd.as_ref() {
            // Optionally substitute variables if not disabled:
            let pwd = if let Some(ref vars) = config.vars
                && !config.disable_var_substitution
            {
                utils::substitute_global_vars(pwd, vars)
            } else {
                pwd.to_owned()
            };
            expr = expr.dir(pwd);
        }

        // Spawn process:
        let handle = expr.unchecked().start()?;
        self.state = ProcessState::Running(Box::new(handle));

        Ok(())
    }

    pub fn spawn(mut self, config: &Config) -> Result<Self, ProcessError> {
        self.spawn_mut(config)?;
        Ok(self)
    }

    #[allow(unused_variables)]
    fn kill(&self, use_taskkill: bool) -> Result<(), ProcessError> {
        if let ProcessState::Running(ref handle) = self.state {
            // Unfortunately, when creating a new console for the process,
            // killing it using the `kill` method won't actually kill it or
            // at least it won't kill all child processes of that child process,
            // which will leave the console running.
            // TASKKILL seems to work though. See: https://stackoverflow.com/a/46429188
            #[cfg(windows)]
            if self.service.create_window.unwrap_or(false) || use_taskkill {
                for pid in handle.pids() {
                    println!(
                        " >>> {}: \"kill\" workaround, running: TASKKILL /F /PID {} /T",
                        self.service.name, pid
                    );
                    duct::cmd!(
                        "cmd.exe",
                        "/C",
                        "TASKKILL",
                        "/F",
                        "/PID",
                        pid.to_string(),
                        "/T"
                    )
                    .run()?;
                }
            }

            handle.kill()?;
        }
        Ok(())
    }
}

pub fn start_services(
    processes: &mut Vec<Process>,
    config: &Config,
    log_path: &str,
    ctrl_c_pressed: Arc<AtomicBool>,
) -> Result<(), ProcessError> {
    println!("Starting services...");
    println!("Press Ctrl+C to kill all processes");

    for service in &config.services {
        if !service.enabled.unwrap_or(true) {
            println!(" >>> {}: skipped (disabled)", service.name);
            continue;
        }
        println!(" >>> {}: starting", service.name);
        match Process::new(service).log_path(log_path).spawn(config) {
            Ok(process) => processes.push(process),
            Err(error) => {
                eprintln!(
                    " >>> ERROR: {} failed to start because {:?}",
                    service.name, error
                );
                if service.required.unwrap_or(false) {
                    eprintln!(
                        " >>> FATAL: required process '{}' didn't start",
                        service.name
                    );
                    return Err(ProcessError::RequiredProcessGone);
                }
            }
        };

        if let Some(milliseconds) = service.wait.or(config.wait) {
            sleep(Duration::from_millis(milliseconds));
        }
        if ctrl_c_pressed.load(Ordering::Relaxed) {
            return Err(ProcessError::CtrlC);
        }
    }
    Ok(())
}

pub fn monitor_processes(
    processes: &mut Vec<Process>,
    config: &Config,
    ctrl_c_pressed: Arc<AtomicBool>,
) -> Result<(), ProcessError> {
    println!("Monitoring services...");
    loop {
        // Keep track of processes that are still running.
        let mut alive = 0;

        // Check each still running process:
        for process in &mut *processes {
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
                    println!(" >>> {}: {} ({})", process.service.name, state, status);

                    let is_required = process.service.required.unwrap_or(false);
                    let has_crashed = !status.success();
                    let restart_policy = process.service.restart.clone().unwrap_or_default();
                    let has_restarted_previously = process.restarted;

                    let should_restart = !has_restarted_previously
                        && (restart_policy == ServiceRestartPolicy::Always
                            || (restart_policy == ServiceRestartPolicy::OnFailure
                                && (is_required || has_crashed)));

                    if should_restart {
                        println!(" >>> Restarting {}", process.service.name);
                        process.restarted = true;
                        if let Err(error) = process.spawn_mut(config) {
                            eprintln!(
                                " >>> ERROR: {} failed to restart because {:?}",
                                process.service.name, error
                            );
                            if is_required {
                                eprintln!(
                                    " >>> FATAL: required process '{}' didn't restart",
                                    process.service.name
                                );
                                return Err(ProcessError::RequiredProcessGone);
                            }
                        };
                    } else {
                        if has_restarted_previously {
                            println!(
                                " >>> Process '{}' has crashed twice, not restarting",
                                process.service.name
                            );
                        }
                        if is_required {
                            println!(
                                " >>> FATAL: required process '{}' {}...",
                                process.service.name, state
                            );
                            return Err(ProcessError::RequiredProcessGone);
                        }
                    }
                }
            }
        }
        if alive == 0 {
            break;
        }
        sleep(Duration::from_millis(1000));
        if ctrl_c_pressed.load(Ordering::Relaxed) {
            return Err(ProcessError::CtrlC);
        }
    }
    Ok(())
}

pub fn run_hook(
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

    println!(
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

pub fn kill_processes(
    processes: &mut Vec<Process>,
    config: &Config,
    log_path: &str,
) -> Result<(), ProcessError> {
    println!("Killing services...");
    for process in processes {
        if let ProcessState::Running(ref handle) = process.state {
            match process.kill(config.use_taskkill) {
                Ok(_) => {
                    let output = handle.wait()?;
                    println!(" >>> {}: killed ({})", process.service.name, output.status);
                }
                Err(error) => eprintln!(
                    " >>> {}: couldn't kill, error: {}",
                    process.service.name, error
                ),
            }
        }
    }
    // Run cleanup hook:
    if let Some(Some(cleanup_hook)) = config.hooks.as_ref().map(|hooks| hooks.cleanup.as_ref()) {
        match run_hook("cleanup", cleanup_hook, log_path, config) {
            Ok(status) => println!(" >>> Hook exited ({})", status),
            Err(error) => eprintln!(" >>> ERROR: Hook failed! {}", error),
        }
    }
    Ok(())
}
