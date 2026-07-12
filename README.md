# Local Runner

**Local Runner** is a CLI tool that runs multiple services defined in a TOML file sequentially. It monitors and restarts services if they exit.
This is useful for local development with multiple dependencies (e.g., web apps, backends, databases) or microservices.

![Demo](demo.svg)

## Features
- Run services sequentially from a TOML configuration.
- Monitors and restarts services on exit (configurable with restart policy: "no", "on-failure", or "always").
- Start, stop, and restart services dynamically while local-runner is already running.
- Define commands, working directories, environment variables, and more per service.
- Resolves dependencies of services when defined and starts them in the correct order.
- Redirects STDOUT/STDERR to log files named after each service.
- Supports environment variable interpolation and Jinja2-like variable substitution.
- Delay the start of subsequent services (e.g. when a service takes longer for initialization)

---

## Installation

### Prerequisites
Install the Rust toolchain using [rustup](https://rustup.rs/).

### Install via Cargo
```bash
cargo install --git https://github.com/FelisDiligens/local-runner.git
```

or if you cloned the project, run:
```bash
cargo install --path .
```
The binary will be installed to `~/.cargo/bin`.

### Uninstall
```bash
cargo uninstall local-runner
```

---

## Usage
After defining your services in a TOML file, run the program:

```bash
# Run when installed:
local-runner --path 'path/to/my/services.toml'
# Run using cargo in project folder:
cargo run -- --path 'path/to/my/services.toml'
```

### Command line arguments
```txt
local-runner [--path PATH] [--logs LOGS] [start|stop|restart SERVICE]
```

```txt
Run multiple services from a TOML file

Usage: local-runner [OPTIONS] [COMMAND]

Commands:
  restart   Restart a specified service from the config file (if daemon is running)
  start     Start a specified service from the config file (if daemon is running)
  stop      Stop a specified service from the config file (if daemon is running)
  status    Prints status of running/exited services (if daemon is running)
  shutdown  Stops all running services (if daemon is running)
  help      Print this message or the help of the given subcommand(s)

Options:
  -p, --path <PATH>  path to config file with services to run [default: ./services.toml]
  -l, --logs <LOGS>  path to folder to write log files to [default: ./]
  -h, --help         Print help
  -V, --version      Print version
```

- If no `--path` is provided, it defaults to `./services.toml`

## Configuration (TOML)

By default, `local-runner` will look for a `services.toml` in the current working directory.

### Example `services.toml`
```toml
# Global settings (optional)
wait = 500  # Default wait time (ms) between services
use_taskkill = false  # Windows-only: Force-kill processes (default: false)
disable_env_interpolation = false  # Disable `$VAR` interpolation (default: enabled)
disable_var_substitution = false  # Disable `{{variable}}` substitution (default: enabled)

# Global environment variables (optional)
env = { "PATH" = "$HOME/.bin:$PATH" }

# Global variables for Jinja2-like substitution (optional)
vars = { "shell" = "sh" }

# Hooks (optional)
[hooks]
prepare = "cp .env.local .env" # Runs before services are started
cleanup = "sh -c \"rm .env && rm -v log-*.txt\""  # Runs after all services exited

# Services (executed top-to-bottom)
[[services]]
name = "hello-world"
cmd = "{{ shell }} -c 'echo Hello, world!'"
pwd = "./"  # Working directory (optional)
cond = "command -v {{ shell }}" # Don't run service if condition returns non-zero exit code (optional)
env = { "NO_COLOR" = "1", "PATH" = "/usr/local/bin:$PATH" }  # Service-specific env (optional)
required = true  # Kill other processes if this crashes (optional)
restart = "on-failure"  # Restart policy: "no", "on-failure", or "always" (optional)
wait = 2000  # Override global wait time (optional)
create_window = true  # Windows-only: Show output in a new console (optional)
depends = ["some-dependency"] # Start services that this service depends on first (optional)

[[services]]
name = "good-bye"
cmd = ["{{ shell }}", "-c", "echo Good bye!"]  # Commands can be lists
state = "disabled" # Disable a service (enabled by default)

[[services]]
name = "some-dependency"
cmd = ["{{ shell }}", "-c", ":"]
```

### Configuration Details

#### Environment Variables
- **Global vs. Service-Specific**: Define `env` globally or per service. Service-specific `env` overrides global values.
- **Syntax**: Use `$VAR` or `${VAR}` syntax to interpolate environment variables. The scope is limited to `env` definitions (so it's not interpolated in commands).
- **Escaping**: Escape `$` with `\$` if needed.

#### Variable Substitution
- **Global Variables**: Define `vars` globally to reuse across services.
- **Syntax**: Use `{{variable}}` or `{{ variable }}` for Jinja2-like substitution in `cmd`, `pwd`, and for hooks.
- **Escaping**: To escape `{{` or `}}`, use `{{ '{{' }}` and `{{ '}}' }}`.

#### Commands
- **Syntax**: Commands are parsed with `shlex` (POSIX syntax), even on Windows.
- **Format**: Can be a string or a list of arguments (e.g., `cmd = ["echo", "Hello"]`).

#### Restart Policies
- `no`: Do not restart (default).
- `on-failure`: Restart only if the exit code > 0.
- `always`: Restart regardless of exit code.

#### States
- `enabled`: Service will autostart (default).
- `disabled`: Service can be manually started.
- `masked`: Service cannot be started at all.

#### Dependencies
- **Execution Order**: The order of the services in the TOML file are respected, except if a service depends on other services. The order will be changed such that it's dependencies are started before it.
- **Service State Behavior**: Services with states `enabled` and `disabled` will be started if another service depends on them. If a service depends on a service which is `masked`, local-runner will abort with an error.
- **Manual Start Behavior**: Dependencies will also be started when manually starting a service.
- **Stopping Behavior**: When a service exits that was a dependency of another service, that's currently unhandled.

#### Conditions
- **Lifecycle**: Runs before starting the service when starting all services.
- **Manual Start Behavior**: Condition is ignored when starting a service manually to allow "overriding" it.

#### Hooks
- **Lifecycle**: Hooks run at specific points (e.g., `cleanup` runs after all services exit).
- **Format**: Can be a string or a list of arguments (same as `cmd`).

#### Windows-Specific Options
- **`use_taskkill`**: Uses `TASKKILL` to force-kill processes and their children. Enable if processes don't terminate properly.
- **`create_window`**: Creates a new console window for the process. No log file is written when enabled.

---

## Development

### Setup
1. Install Rust: [rustup](https://rustup.rs/)
2. Clone the project.

### Commands
```bash
cargo run              # Debug mode
cargo build --release  # Release build
cargo clippy           # Linting
cargo test             # Run tests
```

## License
[MIT](./LICENSE.md)
