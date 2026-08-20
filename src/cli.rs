use anyhow::{Context, Result, bail};
use std::{
    env,
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, ExitStatus, Output},
    thread,
    time::Duration,
};

const SERVICE: &str = "chatgpt-bridge.service";
const BINARY_PATH: &str = "/usr/local/bin/chatgpt-bridge";
const LEGACY_UNINSTALL_PATH: &str = "/usr/local/bin/chatgpt-bridge-uninstall";
const CONFIG_DIR: &str = "/etc/chatgpt-bridge";
const CONFIG_FILE: &str = "/etc/chatgpt-bridge/config.env";
const TLS_DIR: &str = "/etc/chatgpt-bridge/tls";
const TLS_CERT_FILE: &str = "/etc/chatgpt-bridge/tls/fullchain.pem";
const TLS_KEY_FILE: &str = "/etc/chatgpt-bridge/tls/privkey.pem";
const SERVICE_FILE: &str = "/etc/systemd/system/chatgpt-bridge.service";
const PUBLIC_URL_FILE: &str = "/run/chatgpt-bridge/public-url";
const NGROK_AUTH_URL: &str = "https://dashboard.ngrok.com/get-started/your-authtoken";
const DEFAULT_PORT: u16 = 8787;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindMode {
    Local,
    Public,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StartOptions {
    pub workspace: Option<String>,
    pub port: Option<u16>,
    pub mode: Option<BindMode>,
    pub tls_cert: Option<String>,
    pub tls_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliCommand {
    Serve,
    Start(StartOptions),
    Stop,
    Restart,
    Status,
    Logs { follow: bool },
    Key { rotate: bool },
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
        "start" => CliCommand::Start(parse_start(&args[1..])?),
        "stop" => no_extra_args(&args, CliCommand::Stop)?,
        "restart" => no_extra_args(&args, CliCommand::Restart)?,
        "status" => no_extra_args(&args, CliCommand::Status)?,
        "uninstall" => no_extra_args(&args, CliCommand::Uninstall)?,
        "help" | "-h" | "--help" => no_extra_args(&args, CliCommand::Help)?,
        "version" | "-V" | "--version" => no_extra_args(&args, CliCommand::Version)?,
        "key" => match args.as_slice() {
            [_] => CliCommand::Key { rotate: false },
            [_, action] if action == "rotate" => CliCommand::Key { rotate: true },
            _ => bail!("usage: chatgpt-bridge key [rotate]"),
        },
        "logs" => match args.as_slice() {
            [_] => CliCommand::Logs { follow: false },
            [_, flag] if flag == "-f" || flag == "--follow" => CliCommand::Logs { follow: true },
            _ => bail!("usage: chatgpt-bridge logs [-f|--follow]"),
        },
        unknown => bail!("unknown command {unknown:?}; run `chatgpt-bridge help`"),
    };

    Ok(command)
}

fn parse_start(args: &[String]) -> Result<StartOptions> {
    let mut options = StartOptions::default();
    let mut index = 0;

    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let value = next_value(args, &mut index, "--workspace")?;
                set_once(&mut options.workspace, value, "--workspace")?;
            }
            "--port" => {
                let value = next_value(args, &mut index, "--port")?;
                let port = value
                    .parse::<u16>()
                    .with_context(|| format!("invalid port: {value:?}"))?;
                if port < 1024 {
                    bail!(
                        "ports below 1024 are intentionally not used by the unprivileged bridge service; choose 1024-65535"
                    );
                }
                if options.port.replace(port).is_some() {
                    bail!("--port may only be specified once");
                }
            }
            "--public" => set_mode(&mut options, BindMode::Public)?,
            "--local" => set_mode(&mut options, BindMode::Local)?,
            "--tls-cert" => {
                let value = next_value(args, &mut index, "--tls-cert")?;
                set_once(&mut options.tls_cert, value, "--tls-cert")?;
            }
            "--tls-key" => {
                let value = next_value(args, &mut index, "--tls-key")?;
                set_once(&mut options.tls_key, value, "--tls-key")?;
            }
            flag => bail!("unknown start option {flag:?}; run `chatgpt-bridge help`"),
        }
        index += 1;
    }

    if options.tls_cert.is_some() != options.tls_key.is_some() {
        bail!("--tls-cert and --tls-key must be supplied together");
    }

    Ok(options)
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String> {
    *index += 1;
    let value = args
        .get(*index)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{flag} requires a value"))?;
    Ok(value.clone())
}

fn set_once(slot: &mut Option<String>, value: String, flag: &str) -> Result<()> {
    if slot.replace(value).is_some() {
        bail!("{flag} may only be specified once");
    }
    Ok(())
}

fn set_mode(options: &mut StartOptions, mode: BindMode) -> Result<()> {
    if let Some(existing) = options.mode {
        if existing != mode {
            bail!("--public and --local cannot be used together");
        }
        bail!("bind mode may only be specified once");
    }
    options.mode = Some(mode);
    Ok(())
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
        CliCommand::Start(options) => start(&options),
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
        CliCommand::Key { rotate } => manage_key(rotate),
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

fn start(options: &StartOptions) -> Result<()> {
    let mut config = read_config()?;
    let mut changed = false;

    if let Some(workspace) = &options.workspace {
        let workspace = validate_workspace(workspace)?;
        config.set("CHATGPT_BRIDGE_ROOT", &workspace);
        println!("Workspace: {workspace}");
        changed = true;
    } else {
        ensure_workspace_configured_in(&config)?;
    }

    let direct_tls_requested = options.tls_cert.is_some();
    if let (Some(cert), Some(key)) = (&options.tls_cert, &options.tls_key) {
        import_tls(&config, cert, key)?;
        config.set("CHATGPT_BRIDGE_TLS_CERT", TLS_CERT_FILE);
        config.set("CHATGPT_BRIDGE_TLS_KEY", TLS_KEY_FILE);
        config.set("CHATGPT_BRIDGE_NGROK_ENABLED", "false");
        changed = true;
    }

    let current_bind = config
        .value("CHATGPT_BRIDGE_BIND")
        .unwrap_or_else(|| format!("127.0.0.1:{DEFAULT_PORT}"))
        .parse::<SocketAddr>()
        .context("saved CHATGPT_BRIDGE_BIND is invalid")?;

    let port = options.port.unwrap_or(current_bind.port());
    let ip = match options.mode {
        Some(BindMode::Public) if direct_tls_requested => IpAddr::V4(Ipv4Addr::UNSPECIFIED),
        Some(BindMode::Public | BindMode::Local) => IpAddr::V4(Ipv4Addr::LOCALHOST),
        None => current_bind.ip(),
    };
    let bind = SocketAddr::new(ip, port);

    if options.port.is_some() || options.mode.is_some() {
        config.set("CHATGPT_BRIDGE_BIND", &bind.to_string());
        changed = true;
    }

    match options.mode {
        Some(BindMode::Public) if !direct_tls_requested => {
            ensure_ngrok_token(&mut config)?;
            config.set("CHATGPT_BRIDGE_NGROK_ENABLED", "true");
            config.set("CHATGPT_BRIDGE_TLS_CERT", "");
            config.set("CHATGPT_BRIDGE_TLS_KEY", "");
            changed = true;
        }
        Some(BindMode::Public) => {
            config.set("CHATGPT_BRIDGE_NGROK_ENABLED", "false");
            changed = true;
        }
        Some(BindMode::Local) => {
            config.set("CHATGPT_BRIDGE_NGROK_ENABLED", "false");
            config.set("CHATGPT_BRIDGE_TLS_CERT", "");
            config.set("CHATGPT_BRIDGE_TLS_KEY", "");
            changed = true;
        }
        None => {}
    }

    let ngrok_enabled = config.ngrok_enabled();
    if !ngrok_enabled && !bind.ip().is_loopback() && !config.tls_configured() {
        bail!(
            "direct public mode requires HTTPS; either use `--public` for automatic ngrok HTTPS or supply --tls-cert and --tls-key"
        );
    }

    if changed {
        write_config(&config.render())?;
    }

    if ngrok_enabled {
        elevated_best_effort("rm", ["-f", PUBLIC_URL_FILE]);
    }

    elevated_checked("systemctl", ["enable", SERVICE])?;
    if changed {
        elevated_checked("systemctl", ["restart", SERVICE])?;
    } else {
        elevated_checked("systemctl", ["start", SERVICE])?;
    }

    if ngrok_enabled {
        println!("Mode: public (ngrok)");
        println!("Local: http://{bind}");
        if let Some(public_url) = wait_for_public_url() {
            println!("Public: {public_url}");
            println!("GPT Action server: {public_url}");
        } else {
            println!("Public URL is not available yet. Check `chatgpt-bridge logs`.");
        }
    } else {
        let scheme = if config.tls_configured() {
            "https"
        } else {
            "http"
        };
        let visibility = if bind.ip().is_loopback() {
            "local"
        } else {
            "public (direct)"
        };
        println!("Mode: {visibility}");
        println!("Listen: {scheme}://{bind}");
    }

    Ok(())
}

fn ensure_ngrok_token(config: &mut ConfigText) -> Result<()> {
    if config
        .value("NGROK_AUTHTOKEN")
        .is_some_and(|value| !value.trim().is_empty())
    {
        return Ok(());
    }

    println!("\nPublic mode needs a free ngrok account once.");
    println!("Open: {NGROK_AUTH_URL}");
    open_ngrok_auth_page();

    let token = rpassword::prompt_password("Ngrok authtoken: ")?;
    let token = token.trim();
    if token.is_empty() {
        bail!("ngrok authtoken cannot be empty");
    }
    if token.contains('\n') || token.contains('\r') {
        bail!("ngrok authtoken is invalid");
    }

    config.set("NGROK_AUTHTOKEN", token);
    println!("Ngrok account connected. This token will not be requested again.");
    Ok(())
}

fn open_ngrok_auth_page() {
    let result = if env::var_os("WSL_DISTRO_NAME").is_some() {
        Command::new("cmd.exe")
            .args(["/C", "start", "", NGROK_AUTH_URL])
            .spawn()
    } else {
        Command::new("xdg-open").arg(NGROK_AUTH_URL).spawn()
    };

    if result.is_err() {
        // The URL is always printed, so browser launching is only a convenience.
    }
}

fn wait_for_public_url() -> Option<String> {
    for _ in 0..50 {
        if let Ok(value) = fs::read_to_string(PUBLIC_URL_FILE) {
            let value = value.trim();
            if value.starts_with("https://") {
                return Some(value.to_owned());
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
    None
}

fn validate_workspace(workspace: &str) -> Result<String> {
    if workspace.contains('\n') || workspace.contains('\r') {
        bail!("workspace path cannot contain newlines");
    }

    let workspace = fs::canonicalize(workspace)
        .with_context(|| format!("workspace does not exist: {workspace}"))?;
    if !workspace.is_dir() {
        bail!("workspace is not a directory: {}", workspace.display());
    }

    workspace
        .to_str()
        .map(str::to_owned)
        .context("workspace path must be valid UTF-8")
}

fn import_tls(config: &ConfigText, cert: &str, key: &str) -> Result<()> {
    validate_path_arg(cert, "TLS certificate")?;
    validate_path_arg(key, "TLS private key")?;
    elevated_checked("test", ["-f", cert])
        .with_context(|| format!("TLS certificate file does not exist: {cert}"))?;
    elevated_checked("test", ["-f", key])
        .with_context(|| format!("TLS private key file does not exist: {key}"))?;

    let service_user = config
        .value("CHATGPT_BRIDGE_SERVICE_USER")
        .context("CHATGPT_BRIDGE_SERVICE_USER is missing from config; reinstall the bridge")?;
    let group_output = Command::new("id")
        .args(["-gn", service_user.as_str()])
        .output()
        .context("failed to determine service user's primary group")?;
    if !group_output.status.success() {
        bail!("failed to determine service user's primary group");
    }
    let service_group = String::from_utf8(group_output.stdout)
        .context("service group name is not valid UTF-8")?
        .trim()
        .to_owned();

    elevated_checked(
        "install",
        [
            "-d",
            "-m",
            "0750",
            "-o",
            "root",
            "-g",
            service_group.as_str(),
            TLS_DIR,
        ],
    )?;
    elevated_checked(
        "install",
        [
            "-m",
            "0644",
            "-o",
            "root",
            "-g",
            service_group.as_str(),
            cert,
            TLS_CERT_FILE,
        ],
    )?;
    elevated_checked(
        "install",
        [
            "-m",
            "0640",
            "-o",
            "root",
            "-g",
            service_group.as_str(),
            key,
            TLS_KEY_FILE,
        ],
    )?;

    println!("TLS certificate imported.");
    Ok(())
}

fn validate_path_arg(path: &str, label: &str) -> Result<()> {
    if path.is_empty() || path.contains('\n') || path.contains('\r') {
        bail!("{label} path is invalid");
    }
    if !Path::new(path).is_absolute() {
        bail!("{label} path must be absolute");
    }
    Ok(())
}

fn manage_key(rotate: bool) -> Result<()> {
    let mut config = read_config()?;

    if !rotate {
        let token = config
            .value("CHATGPT_BRIDGE_TOKEN")
            .context("CHATGPT_BRIDGE_TOKEN is missing from config")?;
        println!("{token}");
        return Ok(());
    }

    let token = generate_token()?;
    config.set("CHATGPT_BRIDGE_TOKEN", &token);
    write_config(&config.render())?;

    if service_active() {
        elevated_checked("systemctl", ["restart", SERVICE])?;
    }

    println!("Bearer key rotated:");
    println!("{token}");
    println!("Update the key in your Custom GPT Action before the next request.");
    Ok(())
}

fn generate_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    File::open("/dev/urandom")
        .context("failed to open /dev/urandom")?
        .read_exact(&mut bytes)
        .context("failed to read secure random bytes")?;

    let mut token = String::with_capacity(64);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}")?;
    }
    Ok(token)
}

fn ensure_workspace_configured() -> Result<()> {
    let config = read_config()?;
    ensure_workspace_configured_in(&config)
}

fn ensure_workspace_configured_in(config: &ConfigText) -> Result<()> {
    if config
        .value("CHATGPT_BRIDGE_ROOT")
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(());
    }

    bail!("workspace is not configured; run `chatgpt-bridge start --workspace /path/to/projects`")
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

    fn value(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}=");
        self.lines
            .iter()
            .find_map(|line| line.strip_prefix(&prefix))
            .map(parse_env_value)
    }

    fn set(&mut self, name: &str, value: &str) {
        let prefix = format!("{name}=");
        let new_line = format!("{name}={}", env_quote(value));
        if let Some(line) = self.lines.iter_mut().find(|line| line.starts_with(&prefix)) {
            *line = new_line;
        } else {
            self.lines.push(new_line);
        }
    }

    fn tls_configured(&self) -> bool {
        self.value("CHATGPT_BRIDGE_TLS_CERT")
            .is_some_and(|value| !value.is_empty())
            && self
                .value("CHATGPT_BRIDGE_TLS_KEY")
                .is_some_and(|value| !value.is_empty())
    }

    fn ngrok_enabled(&self) -> bool {
        self.value("CHATGPT_BRIDGE_NGROK_ENABLED")
            .is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            })
    }
}

fn parse_env_value(value: &str) -> String {
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        value[1..value.len() - 1]
            .replace("\\\"", "\"")
            .replace("\\\\", "\\")
    } else {
        value.to_owned()
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
  start --workspace PATH             Set workspace and start locally\n\
  start --workspace PATH --port PORT --public\n\
                                     Publish automatically with ngrok HTTPS\n\
  start --public                     Reuse saved workspace/port and publish\n\
  start --public --tls-cert CERT --tls-key KEY\n\
                                     Advanced: direct HTTPS without ngrok\n\
  start --local                      Return to localhost-only HTTP mode\n\
  start                              Start with saved settings\n\
  stop                               Stop the service\n\
  restart                            Restart the service\n\
  status                             Show service status\n\
  logs                               Show latest service logs\n\
  logs -f                            Follow service logs\n\
  key                                Show the Bearer key\n\
  key rotate                         Generate and save a new Bearer key\n\
  uninstall                          Remove the service, config, TLS copy, and binary\n\
  serve                              Run the bridge server (used by systemd)\n\
  version                            Show the installed version\n\
  help                               Show this help\n\n\
The first automatic public start asks for an ngrok authtoken once. The token is\n\
saved in the root-only bridge config. No ngrok binary, router forwarding, TLS\n\
certificate, Nginx, or privileged port is required.",
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

fn service_active() -> bool {
    Command::new("systemctl")
        .args(["is-active", "--quiet", SERVICE])
        .status()
        .is_ok_and(|status| status.success())
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
    use super::{BindMode, CliCommand, StartOptions, parse_args};

    #[test]
    fn no_args_keeps_serve_compatibility() {
        assert_eq!(parse_args(Vec::<String>::new()).unwrap(), CliCommand::Serve);
    }

    #[test]
    fn parses_automatic_public_start() {
        assert_eq!(
            parse_args([
                "start",
                "--workspace",
                "/projects",
                "--port",
                "8787",
                "--public",
            ])
            .unwrap(),
            CliCommand::Start(StartOptions {
                workspace: Some("/projects".to_owned()),
                port: Some(8787),
                mode: Some(BindMode::Public),
                tls_cert: None,
                tls_key: None,
            })
        );
    }

    #[test]
    fn parses_secure_direct_public_start() {
        assert_eq!(
            parse_args([
                "start",
                "--workspace",
                "/projects",
                "--port",
                "8787",
                "--public",
                "--tls-cert",
                "/tmp/fullchain.pem",
                "--tls-key",
                "/tmp/privkey.pem",
            ])
            .unwrap(),
            CliCommand::Start(StartOptions {
                workspace: Some("/projects".to_owned()),
                port: Some(8787),
                mode: Some(BindMode::Public),
                tls_cert: Some("/tmp/fullchain.pem".to_owned()),
                tls_key: Some("/tmp/privkey.pem".to_owned()),
            })
        );
    }

    #[test]
    fn parses_key_commands() {
        assert_eq!(
            parse_args(["key"]).unwrap(),
            CliCommand::Key { rotate: false }
        );
        assert_eq!(
            parse_args(["key", "rotate"]).unwrap(),
            CliCommand::Key { rotate: true }
        );
    }

    #[test]
    fn rejects_unsafe_or_incomplete_start_options() {
        assert!(parse_args(["start", "--port", "443"]).is_err());
        assert!(parse_args(["start", "--public", "--local"]).is_err());
        assert!(parse_args(["start", "--tls-cert", "/tmp/cert.pem"]).is_err());
        assert!(parse_args(["start", "--nope"]).is_err());
    }
}
