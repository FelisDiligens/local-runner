use std::{collections::HashMap, fs};

use clap::Parser;
use serde::Deserialize;

use crate::{error::ConfigError, utils};

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

#[derive(Deserialize, Debug, Clone)]
pub struct Service {
    /// A pretty name for the service shown in the cli output
    pub name: String,
    /// A command parsed like in a POSIX shell (using `shlex`)
    pub cmd: Command,
    /// The working directory of the invoked command, defaults to pwd of service runner
    pub pwd: Option<String>,
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
    /// If false, skips starting this service (defaults to true)
    pub enabled: Option<bool>,
}

pub fn load_config(config_path: &str) -> Result<Config, ConfigError> {
    log::info!("Loading config from {}...", config_path);
    let raw_toml = fs::read_to_string(config_path)?;
    let config: Config = toml::from_str(&raw_toml)?;
    Ok(config)
}
