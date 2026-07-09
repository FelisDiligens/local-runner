use std::path::PathBuf;

use duct::Handle;

use crate::config::{Config, Service};
use crate::error::ProcessError;
use crate::utils;

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
    pub fn kill(&self, use_taskkill: bool) -> Result<(), ProcessError> {
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
