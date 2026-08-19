use anyhow::{Context, Result, bail};
use std::{
    env,
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    process::{Command, ExitStatus, Output},
};

const SERVICE: &str = "chatgpt-bridge.service";
const BINARY_PATH: &str = "/usr/local/bin/chatgpt-bridge";
const LEGACY_UNINSTALL_PATH: &str = "/usr/local/bin/chatgpt-bridge-uninstall";
const CONFIG_DIR: &str = "/etc/chatgpt-bridge";
const CONFIG_FILE: &str = "/etc/chatgpt-bridge/config.env";
const SERVICE_FILE: &str = "/etc/systemd/system/chatgpt-bridge.service";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Serve,
    Start { workspace: Option<String> },
    Stop,
    Restart,
    Status,
    Logs { follow: bool },
    Uninstall,
    Help,
    Version,
}

pub fn parse_args<I, S>(args: I) -> Result<CliCommand>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args = args
        .into_iter()
        .map(|arg| arg.as_ref().to_owned())
        .collect::<Vec<_>>();

    if args.is_empty() {
        return Ok(CliCommand::Serve);
    }

    let command = match args[0].as_str() {
        "serve" => no_extra_args(&args, CliCommand::Serve)?,
        "start" => match args.as_slice() {
            [_] => CliCommand::Start { workspace: None },
            [_, flag, workspace] if flag == "--workspace" && !workspace.is_empty() => {
                CliCommand::Start {
                    workspace: Some(workspace.clone()),
                }
            }
            _ => bail!("usage: chatgpt-bridge start [--workspace PATH]"),
        },
        "stop" => no_extra_args(&args, CliCommand::Stop)?,
        "restart" => no_extra_args(&args, CliCommand::Restart)?,
        "status" => no_extra_args(&args, CliCommand::Status)?,
        "uninstall" => no_extra_args(&args, CliCommand::Uninstall)?,
        "help" | "-h" | "--help" => no_extra_args(&args, CliCommand::Help)?,
        "version" | "-V" | "--version" => no_extra_args(&args, CliCommand::Version)?,
        "logs" => match args.as_slice() {
            [_] => CliCommand::Logs { follow: false },
            [_, flag] if flag == "-f" || flag == "--follow" => CliCommand::Logs { follow: true },
            _ => bail!("usage: chatgpt-bridge logs [-f|--follow]"),
        },
        unknown => bail!("unknown command {unknown:?}; run `chatgpt-bridge help`"),
    };

    Ok(command)
}

fn no_extra_args(args: &[String], command: CliCommand) -> Result<CliCommand> {
    if args.len() != 1 {
        bail!("unexpected arguments; run `chatgpt-bridge help`");
    }
    Ok(command)
}

pub fn execute(command: CliCommand) -> Result<()> {
    match command {
        CliCommand::Serve => bail!("serve must be handled by the server runtime"),
        CliCommand::Start { workspace } => start(workspace.as_deref()),
        CliCommand::Stop => elevated_checked("systemctl", ["stop", SERVICE]),
        CliCommand::Restart => {
            ensure_workspace_configured()?;
            elevated_checked("systemctl", ["restart", SERVICE])
        }
        CliCommand::Status => checked("systemctl", ["--no-pager", "--full", "status", SERVICE]),
        CliCommand::Logs { follow } => {
            if follow {
                elevated_checked("journalctl", ["-u", SERVICE, "-f"])
            } else {
                elevated_checked("journalctl", ["-u", SERVICE, "-n", "100", "--no-pager"])
            }
        }
        CliCommand::Uninstall => uninstall(),
        CliCommand::Help => {
            print_help();
            Ok(())
        }
        CliCommand::Version => {
            println!("chatgpt-bridge {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
    }
}

fn start(workspace: Option<&str>) -> Result<()> {
    if let Some(workspace) = workspace {
        set_workspace(workspace)?;
    } else {
        ensure_workspace_configured()?;
    }

    elevated_checked("systemctl", ["enable", SERVICE])?;

    if workspace.is_some() {
        elevated_checked("systemctl", ["restart", SERVICE])
    } else {
        elevated_checked("systemctl", ["start", SERVICE])
    }
}

fn set_workspace(workspace: &str) -> Result<()> {
    if workspace.contains('\n') || workspace.contains('\r') {
        bail!("workspace path cannot contain newlines");
    }

    let workspace = fs::canonicalize(workspace)
        .with_context(|| format!("workspace does not exist: {workspace}"))?;
    if !workspace.is_dir() {
        bail!("workspace is not a directory: {}", workspace.display());
    }

    let workspace = workspace
        .to_str()
        .context("workspace path must be valid UTF-8")?;
    let mut config = read_config()?;
    let root_line = format!("CHATGPT_BRIDGE_ROOT={}", env_quote(workspace));

    if let Some(line) = config
        .lines
        .iter_mut()
        .find(|line| line.starts_with("CHATGPT_BRIDGE_ROOT="))
    {
        *line = root_line;
    } else {
        config.lines.push(root_line);
    }

    write_config(&config.render())?;
    println!("Workspace: {workspace}");
    Ok(())
}

fn ensure_workspace_configured() -> Result<()> {
    let config = read_config()?;
    let configured = config.lines.iter().any(|line| {
        line.strip_prefix("CHATGPT_BRIDGE_ROOT=")
            .is_some_and(|value| !value.trim_matches('"').is_empty())
    });

    if !configured {
        bail!(
            "workspace is not configured; run `chatgpt-bridge start --workspace /path/to/projects`"
        );
    }

    Ok(())
}

#[derive(Debug)]
struct ConfigText {
    lines: Vec<String>,
}

impl ConfigText {
    fn render(&self) -> String {
        let mut text = self.lines.join("\n");
        text.push('\n');
        text
    }
}

fn read_config() -> Result<ConfigText> {
    let output = elevated_output("cat", [CONFIG_FILE])?;
    let text = String::from_utf8(output.stdout).context("config file is not valid UTF-8")?;
    Ok(ConfigText {
        lines: text.lines().map(str::to_owned).collect(),
    })
}

fn write_config(content: &str) -> Result<()> {
    let temp_path = env::temp_dir().join(format!(
        "chatgpt-bridge-config-{}-{}",
        std::process::id(),
        unique_suffix()
    ));

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp_path)
            .with_context(|| format!("failed to create {}", temp_path.display()))?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;

        elevated_checked(
            "install",
            [
                OsStr::new("-m"),
                OsStr::new("0600"),
                temp_path.as_os_str(),
                OsStr::new(CONFIG_FILE),
            ],
        )
    })();

    let _ = fs::remove_file(&temp_path);
    result
}

fn unique_suffix() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

fn env_quote(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn print_help() {
    println!(
        "ChatGPT Bridge {}\n\n\
Usage:\n\
  chatgpt-bridge <command>\n\n\
Commands:\n\
  start --workspace PATH   Set workspace and start the service\n\
  start                    Start with the saved workspace\n\
  stop                     Stop the systemd service\n\
  restart                  Restart with the saved workspace\n\
  status                   Show service status\n\
  logs                     Show the latest 100 service log lines\n\
  logs -f                  Follow service logs\n\
  uninstall                Remove the service, config, and binary\n\
  serve                    Run the HTTP bridge server (used by systemd)\n\
  version                  Show the installed version\n\
  help                     Show this help",
        env!("CARGO_PKG_VERSION")
    );
}

fn uninstall() -> Result<()> {
    println!("Removing ChatGPT Bridge...");

    elevated_best_effort("systemctl", ["stop", SERVICE]);
    elevated_best_effort("systemctl", ["disable", SERVICE]);

    elevated_checked("rm", ["-f", SERVICE_FILE])?;
    elevated_checked("rm", ["-rf", CONFIG_DIR])?;
    elevated_checked("rm", ["-f", LEGACY_UNINSTALL_PATH])?;

    elevated_checked("systemctl", ["daemon-reload"])?;
    elevated_best_effort("systemctl", ["reset-failed", SERVICE]);

    elevated_checked("rm", ["-f", BINARY_PATH])?;

    println!(
        "ChatGPT Bridge was removed. The configured workspace and project files were left untouched."
    );
    Ok(())
}

fn is_root() -> Result<bool> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .context("failed to run `id -u`")?;
    if !output.status.success() {
        bail!("`id -u` failed");
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim() == "0")
}

fn checked<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("failed to run {program}"))?;
    ensure_success(program, status)
}

fn elevated_output<I, S>(program: &str, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let output = if is_root()? {
        Command::new(program).args(&args).output()
    } else {
        Command::new("sudo").arg(program).args(&args).output()
    }
    .with_context(|| format!("failed to run privileged command {program}"))?;

    if !output.status.success() {
        ensure_success(program, output.status)?;
    }
    Ok(output)
}

fn elevated_checked<I, S>(program: &str, args: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let status = if is_root()? {
        Command::new(program).args(&args).status()
    } else {
        Command::new("sudo").arg(program).args(&args).status()
    }
    .with_context(|| format!("failed to run privileged command {program}"))?;

    ensure_success(program, status)
}

fn elevated_best_effort<I, S>(program: &str, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args = args.into_iter().collect::<Vec<_>>();
    let result = if is_root().unwrap_or(false) {
        Command::new(program).args(&args).status()
    } else {
        Command::new("sudo").arg(program).args(&args).status()
    };

    if let Err(error) = result {
        eprintln!("warning: failed to run {program}: {error}");
    }
}

fn ensure_success(program: &str, status: ExitStatus) -> Result<()> {
    if status.success() {
        return Ok(());
    }

    match status.code() {
        Some(code) => bail!("{program} exited with status {code}"),
        None => bail!("{program} terminated by signal"),
    }
}

#[cfg(test)]
mod tests {
    use super::{CliCommand, parse_args};

    #[test]
    fn no_args_keeps_serve_compatibility() {
        assert_eq!(parse_args(Vec::<String>::new()).unwrap(), CliCommand::Serve);
    }

    #[test]
    fn parses_start_with_or_without_workspace() {
        assert_eq!(
            parse_args(["start"]).unwrap(),
            CliCommand::Start { workspace: None }
        );
        assert_eq!(
            parse_args(["start", "--workspace", "/projects"]).unwrap(),
            CliCommand::Start {
                workspace: Some("/projects".to_owned())
            }
        );
    }

    #[test]
    fn parses_service_commands() {
        assert_eq!(parse_args(["stop"]).unwrap(), CliCommand::Stop);
        assert_eq!(parse_args(["restart"]).unwrap(), CliCommand::Restart);
        assert_eq!(parse_args(["status"]).unwrap(), CliCommand::Status);
        assert_eq!(parse_args(["uninstall"]).unwrap(), CliCommand::Uninstall);
    }

    #[test]
    fn parses_log_follow_flag() {
        assert_eq!(
            parse_args(["logs", "--follow"]).unwrap(),
            CliCommand::Logs { follow: true }
        );
        assert_eq!(
            parse_args(["logs"]).unwrap(),
            CliCommand::Logs { follow: false }
        );
    }

    #[test]
    fn rejects_unknown_or_extra_arguments() {
        assert!(parse_args(["nope"]).is_err());
        assert!(parse_args(["start", "extra"]).is_err());
        assert!(parse_args(["start", "--workspace"]).is_err());
        assert!(parse_args(["logs", "--bad"]).is_err());
    }
}
