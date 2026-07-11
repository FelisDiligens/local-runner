//! The most "simple stupid" IPC. Place a text file next to the config file...

use std::path::{Path, PathBuf};
use std::{fs, io};

use crate::utils::write_to_file;

pub fn get_ipc_file<P: AsRef<Path>>(path: P) -> PathBuf {
    let file_name = path
        .as_ref()
        .file_name()
        .expect("file_name to be Some")
        .to_string_lossy()
        .to_string();

    let file_name = format!(".{file_name}.ipc");
    path.as_ref().with_file_name(file_name)
}

pub fn read_message<P: AsRef<Path>>(path: P) -> io::Result<Option<String>> {
    let ipc_file = get_ipc_file(path);
    fs::read_to_string(ipc_file).map(|s| {
        if s.trim().is_empty() {
            None
        } else {
            Some(s.trim().to_string())
        }
    })
}

pub fn write_message<S: AsRef<str>, P: AsRef<Path>>(msg: S, path: P) -> io::Result<()> {
    let ipc_file = get_ipc_file(path);
    write_to_file(ipc_file, msg)
}

pub fn delete_ipc_file<P: AsRef<Path>>(path: P) -> io::Result<()> {
    let ipc_file = get_ipc_file(path);
    fs::remove_file(ipc_file)
}
