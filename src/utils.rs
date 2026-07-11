use std::collections::HashMap;
use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use colored::Colorize;
use regex::{Captures, Regex};

/// SIGINT (Ctrl+C) is sent to all processes in the "foreground group".  
/// To prevent this, the spawned process should be in it's own process group.
///
/// Sources:
/// [Unix & Linux Stack Exchange](https://unix.stackexchange.com/a/149756),
/// [Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags),
/// [duct.rs #70](https://github.com/oconnor663/duct.rs/issues/70)
pub fn create_new_process_group(cmd: &mut std::process::Command) -> Result<(), io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;

        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP);
        Ok(())
    }
    #[cfg(unix)]
    {
        use nix::unistd::{Pid, setpgid};
        use std::os::unix::process::CommandExt;

        unsafe {
            cmd.pre_exec(|| match setpgid(Pid::from_raw(0), Pid::from_raw(0)) {
                Ok(()) => Ok(()),
                Err(errno) => Err(io::Error::from_raw_os_error(errno as _)),
            });
        }
        Ok(())
    }
    #[cfg(not(any(windows, unix)))]
    {
        unimplemented!("create_new_process_group is not implemented for this OS!");
    }
}

#[cfg_attr(not(windows), allow(unused_variables))]
pub fn create_new_console(cmd: &mut std::process::Command) -> Result<(), io::Error> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x00000010;

        cmd.creation_flags(CREATE_NEW_CONSOLE);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        unimplemented!("create_new_console is not implemented for this OS!");
    }
}

pub fn register_ctrlc_handler() -> Arc<AtomicBool> {
    // Handle Ctrl+C by storing it in a shared boolean:
    let ctrl_c_pressed = Arc::new(AtomicBool::new(false));
    ctrlc::set_handler({
        let ctrl_c_pressed = ctrl_c_pressed.clone();
        move || {
            #[cfg(windows)]
            println!("^C");
            #[cfg(not(windows))]
            println!();
            ctrl_c_pressed.store(true, Ordering::Relaxed);
        }
    })
    .expect("Error setting Ctrl+C handler");

    ctrl_c_pressed
}

pub fn setup_stdout_logger() -> Result<(), fern::InitError> {
    fern::Dispatch::new()
        .format(move |out, message, record| {
            let message = match record.level() {
                log::Level::Error => message.to_string().red(),
                log::Level::Warn => message.to_string().yellow(),
                log::Level::Info => message.to_string().into(),
                log::Level::Debug => message.to_string().white(),
                log::Level::Trace => message.to_string().white(),
            };
            out.finish(format_args!("{}", message))
        })
        .chain(std::io::stdout())
        .apply()?;
    Ok(())
}

/// Expands environment variables in given string like in a Unix shell.
/// All occurences of `$VAR` and `${VAR}` will be replaced by the environment variables value or an empty string.
pub fn expand_env_vars(s: &str) -> String {
    // Regex matches any occurence of `$VAR`, `${VAR}`, `\$VAR`, and `\${VAR}`:
    let re = Regex::new(r#"\\{0,2}\$\{?(?P<name>[a-zA-Z0-9_]+)\}?"#).expect("couldn't parse regex");

    let s = re.replace_all(s, |captures: &Captures<'_>| {
        // Unwrap is fine, because: "When i == 0, this is guaranteed to return a non-None value.":
        let capture = captures.get(0).map(|m| m.as_str()).unwrap();
        // Replace environment variable in match, unless prefixed with backslash or invalid syntax:
        let name = captures.name("name").map(|m| m.as_str());
        if let Some(name) = name
            && !capture.starts_with(r"\$")
            && capture.contains("{") == capture.contains("}")
        {
            let val = env::var(name).unwrap_or_default(); // emulate shell behavior by replacing with empty string if variable is unset.
            // Replace double backslash with single backslash:
            if capture.starts_with(r"\\") {
                return r"\".to_string() + &val;
            } else {
                return val;
            }
        } else if capture.starts_with(r"\$") {
            // Remove escaping backslash, leaving only the $ sign:
            return capture[1..].to_string();
        }
        capture.to_string()
    });

    s.to_string()
}

/// Substitutes global variables in given string kind of like in Jinja2.
/// All occurences of `{{VAR}}` and `{{ VAR }}` will be replaced by the variable's value if found.
/// In cases where we need to escape the `{{` or `}}`, we can use `{{ '{{' }}` and `{{ '}}' }}`,
/// but this shouldn't generally be needed as long as the variable name is sanely chosen.
pub fn substitute_global_vars(s: &str, vars: &HashMap<String, String>) -> String {
    // Regex matches any occurence of `{{VAR}}`, `{{ VAR }}` and `{{ '{{' }}`
    let re = Regex::new(r#"\{{2}\s?['"]?(?P<name_or_str>[a-zA-Z0-9_\{\}]+)['"]?\s?\}{2}"#)
        .expect("couldn't parse regex");

    let s = re.replace_all(s, |captures: &Captures<'_>| {
        // Unwrap is fine, because: "When i == 0, this is guaranteed to return a non-None value.":
        let capture = captures.get(0).map(|m| m.as_str()).unwrap();
        // Replace global variable in match, unless the variable doesn't exist:
        let name_or_str = captures.name("name_or_str").map(|m| m.as_str());
        if let Some(name_or_str) = name_or_str {
            // Treat as a string if single or double quotes are found:
            if (capture.starts_with("{{ '")
                || capture.starts_with("{{ \"")
                || capture.starts_with("{{'")
                || capture.starts_with("{{\""))
                && (capture.ends_with("' }}")
                    || capture.ends_with("\" }}")
                    || capture.ends_with("'}}")
                    || capture.ends_with("\"}}"))
            {
                return name_or_str.to_string();
            }

            // Treat as a variable if it exists:
            if let Some(value) = vars.get(name_or_str) {
                return value.clone();
            }
        }
        // Fallback to entire captured string:
        capture.to_string()
    });

    s.to_string()
}

/// Writes the string to a file, creating one if it doesn't exist yet.
/// This is a convenience function for using `OpenOptions::open` and `write!`.
pub fn write_to_file<P: AsRef<Path>, S: AsRef<str>>(path: P, content: S) -> io::Result<()> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    write!(&file, "{}", content.as_ref())?;
    Ok(())
}
