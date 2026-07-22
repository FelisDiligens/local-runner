use std::{collections::HashMap, fs};

use clap::{Parser, Subcommand};
use serde::Deserialize;

use crate::{error::ConfigError, utils};

#[derive(Subcommand, Debug, Clone)]
pub enum CommandArgs {
    /// Restart a specified service from the config file (if daemon is running)
    Restart {
        /// Name of the service to restart
        service: String,
    },
    /// Start a specified service (or multiple via tag) from the config file (if daemon is running)
    Start {
        /// Whether or not the argument should be interpreted as a tag
        #[arg(short, long, default_value_t = false)]
        tag: bool,
        /// Name of the service or tag to start
        service_or_tag: String,
    },
    /// Stop a specified service from the config file (if daemon is running)
    Stop {
        /// Name of the service to stop
        service: String,
    },
    /// Prints status of running/exited services (if daemon is running)
    Status,
    /// Stops all running services (if daemon is running)
    Shutdown,
}

/// Run multiple services from a TOML file.
#[derive(Parser, Debug, Clone)]
#[command(version, about, long_about = None)]
pub struct Args {
    /// path to config file with services to run
    #[arg(short, long, default_value_t = String::from("./services.toml"))]
    pub path: String,
    /// path to folder to write log files to
    #[arg(short, long, default_value_t = String::from("./"))]
    pub logs: String,
    #[command(subcommand)]
    pub command: Option<CommandArgs>,
}

#[derive(Deserialize, Debug, Default, Clone)]
pub struct Config {
    /// Default wait time in milliseconds before starting next process
    pub wait: Option<u64>,
    pub services: Vec<Service>,
    pub hooks: Option<Hooks>,
    /// Environment variables to set for all services
    pub env: Option<HashMap<String, String>>,
    /// Global variables that can be used in commands
    pub vars: Option<HashMap<String, String>>,
    /// (Windows only) always kill using `TASKKILL`, defaults to false
    #[serde(default)]
    pub use_taskkill: bool,
    /// Disable environment `$VAR`/`${VAR}` interpolation inside of environment variables
    #[serde(default)]
    pub disable_env_interpolation: bool,
    /// Disable environment `{{variable}}`/`{{ variable }}` substitution inside of commands
    #[serde(default)]
    pub disable_var_substitution: bool,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Command {
    String(String),
    Array(Vec<String>),
}

impl Command {
    pub fn parse(&self) -> Option<Vec<String>> {
        match self {
            Command::String(str) => shlex::split(str),
            Command::Array(vec) => Some(vec.clone()),
        }
    }

    pub fn parse_with_subst(&self, vars: &HashMap<String, String>) -> Option<Vec<String>> {
        match self {
            Command::String(str) => shlex::split(&utils::substitute_global_vars(str, vars)),
            Command::Array(vec) => Some(
                vec.iter()
                    .map(|s| utils::substitute_global_vars(s, vars))
                    .collect(),
            ),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct Hooks {
    /// runs before services are started
    pub prepare: Option<Command>,
    /// runs after all services were killed
    pub cleanup: Option<Command>,
}

#[derive(Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceRestartPolicy {
    #[default]
    No,
    OnFailure,
    Always,
}

#[derive(Deserialize, Default, Debug, Clone, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceState {
    #[default]
    Enabled,
    Disabled,
    Masked,
}

#[derive(Deserialize, Debug, Clone)]
pub struct Service {
    /// The name of the service shown in the CLI output, also used for the `depends` field
    pub name: String,
    /// A command parsed like in a POSIX shell (using `shlex`)
    pub cmd: Command,
    /// The working directory of the invoked command, defaults to pwd of service runner
    pub pwd: Option<String>,
    /// A command whose exit code determines whether the service is started or not
    pub cond: Option<Command>,
    /// Additional environment variables to set for the invoked process
    pub env: Option<HashMap<String, String>>,
    /// Kill other processes when this process crashes
    pub required: Option<bool>,
    /// Restart process when it crashes (`required` needs to be false/unset)
    pub restart: Option<ServiceRestartPolicy>,
    /// Wait time in milliseconds before starting next process
    pub wait: Option<u64>,
    /// (Windows only) The new process has a new console, instead of inheriting its parent's console
    pub create_window: Option<bool>,
    /// If "disabled", skips starting this service (defaults to "enabled")
    /// If "masked", any attempt at starting this service will fail (e.g. manually starting it)
    pub state: Option<ServiceState>,
    /// List of tags that can be used to start multiple related services
    pub tags: Option<Vec<String>>,
    /// List of services that this service depends on
    pub depends: Option<Vec<String>>,
}

pub fn load_config(config_path: &str) -> Result<Config, ConfigError> {
    log::info!("Loading config from {}...", config_path);
    let raw_toml = fs::read_to_string(config_path)?;
    let config: Config = toml::from_str(&raw_toml)?;
    Ok(config)
}
