use std::io;

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("couldn't load TOML file")]
    IOError(#[from] io::Error),
    #[error("couldn't parse TOML file")]
    ParseError(#[from] toml::de::Error),
}

#[derive(Error, Debug)]
pub enum DependencyError {
    #[error("service '{0}' not in list")]
    UnknownDependency(String),
    #[error("one or more dependencies are masked")]
    MaskedDepedencies,
    #[error("dependencies are cyclic")]
    CyclicDepedencies,
}

#[derive(Error, Debug)]
pub enum ProcessError {
    #[error(transparent)]
    IOError(#[from] io::Error),
    #[error("required process is gone")]
    RequiredProcessGone,
    #[error("manually stopped such as by keyboard interrupt")]
    ManuallyStopped,
    #[error("all services stopped")]
    AllServicesStopped,
    #[error("command for {0} couldn't be parsed")]
    CommandParse(String),
}

#[derive(Error, Debug)]
pub enum WorkerError {
    #[error(transparent)]
    IOError(#[from] io::Error),
    #[error("required process is gone")]
    RequiredProcessGone,
    #[error("manually stopped such as by keyboard interrupt")]
    ManuallyStopped,
    #[error("all services stopped")]
    AllServicesStopped,
    #[error("command for {0} couldn't be parsed")]
    CommandParse(String),
    #[error("Message queue disconnected")]
    Disconnected,
}

impl From<ProcessError> for WorkerError {
    fn from(value: ProcessError) -> Self {
        match value {
            ProcessError::IOError(error) => WorkerError::IOError(error),
            ProcessError::RequiredProcessGone => WorkerError::RequiredProcessGone,
            ProcessError::ManuallyStopped => WorkerError::ManuallyStopped,
            ProcessError::AllServicesStopped => WorkerError::AllServicesStopped,
            ProcessError::CommandParse(s) => WorkerError::CommandParse(s),
        }
    }
}
