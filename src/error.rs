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
pub enum ProcessError {
    #[error("io::Error")]
    IOError(#[from] io::Error),
    #[error("required process is gone")]
    RequiredProcessGone,
    #[error("keyboard interrupt pressed")]
    CtrlC,
    #[error("command for {0} couldn't be parsed")]
    CommandParse(String),
}
