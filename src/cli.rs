use anyhow::{Context, Result, bail};
use std::{
    env,
    ffi::OsStr,
    process::{Command, ExitStatus},
};

const SERVICE: &str = "chatgpt-bridge.service";
const BINARY_PATH: &str = "/usr/local/bin/chatgpt-bridge";
const LEGACY_UNINSTALL_PATH: &str = "/usr/local/bin/chatgpt-bridge-uninstall";
const CONFIG_DIR: &str = "/etc/chatgpt-bridge";
const SERVICE_FILE: &str = "/etc/systemd/system/chatgpt-bridge.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliCommand {
    Serve,
    Start,
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
        // Keep no-argument startup compatible with early installations.
        return Ok(CliCommand::Serve);
    }

    let command = match args[0].as_str() {
        "serve" => no_extra_args(&args, CliCommand::Serve)?,
        "start" => no_extra_args(&args, CliCommand::Start)?,
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
        CliCommand::Start => elevated_checked("systemctl", ["start", SERVICE]),
        CliCommand::Stop => elevated_checked("systemctl", ["stop", SERVICE]),
        CliCommand::Restart => elevated_checked("systemctl", ["restart", SERVICE]),
        CliCommand::Status => checked(
            "systemctl",
            ["--no-pager", "--full", "status", SERVICE],
        ),
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

fn print_help() {
    println!(
        "ChatGPT Bridge {}\n\n\
Usage:\n\
  chatgpt-bridge <command>\n\n\
Commands:\n\
  start       Start the systemd service\n\
  stop        Stop the systemd service\n\
  restart     Restart the systemd service\n\
  status      Show service status\n\
  logs        Show the latest 100 service log lines\n\
  logs -f     Follow service logs\n\
  uninstall   Remove the installed service, config, and binary\n\
  serve       Run the HTTP bridge server (normally used by systemd)\n\
  version     Show the installed version\n\
  help        Show this help\n\n\
The installer is ./install.sh. After installation, normal service management\n\
uses this single chatgpt-bridge command.",
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

    // Remove the currently executing binary last. Linux keeps the running
    // executable mapped until this process exits.
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
    fn parses_service_commands() {
        assert_eq!(parse_args(["start"]).unwrap(), CliCommand::Start);
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
        assert!(parse_args(["logs", "--bad"]).is_err());
    }
}
