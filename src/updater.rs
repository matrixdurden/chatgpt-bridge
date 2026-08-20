use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::{
    env,
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, ExitStatus, Output},
    time::{SystemTime, UNIX_EPOCH},
};

const REPOSITORY: &str = "matrixdurden/chatgpt-bridge";
const BINARY_PATH: &str = "/usr/local/bin/chatgpt-bridge";
const SERVICE: &str = "chatgpt-bridge.service";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpdateOptions {
    pub check: bool,
    pub version: Option<String>,
}

pub fn run_args(args: &[String]) -> Result<()> {
    let options = parse_args(args)?;
    run(&options)
}

fn parse_args(args: &[String]) -> Result<UpdateOptions> {
    match args {
        [] => Ok(UpdateOptions::default()),
        [flag] if flag == "--check" => Ok(UpdateOptions {
            check: true,
            version: None,
        }),
        [flag, version] if flag == "--version" => Ok(UpdateOptions {
            check: false,
            version: Some(version.clone()),
        }),
        [flag] if flag == "-h" || flag == "--help" => {
            println!(
                "Usage:\n  chatgpt-bridge update\n  chatgpt-bridge update --check\n  chatgpt-bridge update --version VERSION"
            );
            Ok(UpdateOptions {
                check: true,
                version: Some(format!("v{}", env!("CARGO_PKG_VERSION"))),
            })
        }
        _ => bail!(
            "usage: chatgpt-bridge update [--check | --version VERSION]"
        ),
    }
}

pub fn run(options: &UpdateOptions) -> Result<()> {
    let current_tag = format!("v{}", env!("CARGO_PKG_VERSION"));
    let target_tag = match &options.version {
        Some(version) => normalize_tag(version)?,
        None => latest_release_tag()?,
    };

    if options.check {
        println!("Current: {}", current_tag.trim_start_matches('v'));
        println!("Latest:  {}", target_tag.trim_start_matches('v'));
        if target_tag == current_tag {
            println!("Status:  up to date");
        } else {
            println!("Status:  update available");
            println!("Run:     chatgpt-bridge update");
        }
        return Ok(());
    }

    if options.version.is_none() && target_tag == current_tag {
        println!(
            "chatgpt-bridge {} is already up to date.",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }

    println!(
        "Updating chatgpt-bridge {} -> {}...",
        current_tag.trim_start_matches('v'),
        target_tag.trim_start_matches('v')
    );
    install_release(&target_tag)?;
    println!(
        "Updated to chatgpt-bridge {}.",
        target_tag.trim_start_matches('v')
    );
    Ok(())
}

fn latest_release_tag() -> Result<String> {
    let api = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let output = checked_output(
        "curl",
        [
            "-fsSL",
            "--retry",
            "3",
            "--connect-timeout",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: chatgpt-bridge",
            api.as_str(),
        ],
    )
    .context(
        "failed to query the latest GitHub release; is curl installed and is the network reachable?",
    )?;

    let json: Value = serde_json::from_slice(&output.stdout)
        .context("GitHub returned an invalid release response")?;
    let tag = json
        .get("tag_name")
        .and_then(Value::as_str)
        .context("GitHub release response did not include tag_name")?;
    normalize_tag(tag)
}

fn normalize_tag(version: &str) -> Result<String> {
    let version = version.trim();
    if version.is_empty() {
        bail!("version cannot be empty");
    }

    let raw = version.strip_prefix('v').unwrap_or(version);
    if raw.is_empty()
        || !raw
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '+'))
    {
        bail!("invalid version {version:?}");
    }

    Ok(format!("v{raw}"))
}

fn install_release(tag: &str) -> Result<()> {
    let target = release_target()?;
    let asset = format!("chatgpt-bridge-{target}.tar.gz");
    let base = format!("https://github.com/{REPOSITORY}/releases/download/{tag}");

    let temp_dir = env::temp_dir().join(format!(
        "chatgpt-bridge-update-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    fs::create_dir(&temp_dir)
        .with_context(|| format!("failed to create {}", temp_dir.display()))?;

    let result = (|| -> Result<()> {
        let archive = temp_dir.join(&asset);
        let sums = temp_dir.join("SHA256SUMS");

        download(&format!("{base}/{asset}"), &archive)?;
        download(&format!("{base}/SHA256SUMS"), &sums)?;
        verify_checksum(&archive, &sums, &asset)?;

        checked(
            "tar",
            [
                OsStr::new("-xzf"),
                archive.as_os_str(),
                OsStr::new("-C"),
                temp_dir.as_os_str(),
            ],
        )
        .context("failed to extract update archive")?;

        let candidate = temp_dir.join("chatgpt-bridge");
        if !candidate.is_file() {
            bail!("release archive did not contain chatgpt-bridge");
        }
        verify_binary_version(&candidate, tag)?;

        let was_active = Command::new("systemctl")
            .args(["is-active", "--quiet", SERVICE])
            .status()
            .is_ok_and(|status| status.success());

        elevated_checked(
            "install",
            [
                OsStr::new("-m"),
                OsStr::new("0755"),
                candidate.as_os_str(),
                OsStr::new(BINARY_PATH),
            ],
        )
        .context("failed to replace installed chatgpt-bridge binary")?;

        if was_active {
            elevated_checked("systemctl", ["restart", SERVICE])
                .context("binary updated, but the service could not be restarted")?;
        }

        Ok(())
    })();

    let _ = fs::remove_dir_all(&temp_dir);
    result
}

fn release_target() -> Result<&'static str> {
    if env::consts::OS != "linux" {
        bail!("self-update currently supports Linux only");
    }

    match env::consts::ARCH {
        "x86_64" => Ok("x86_64-unknown-linux-gnu"),
        "aarch64" => Ok("aarch64-unknown-linux-gnu"),
        arch => bail!("no prebuilt release is available for Linux architecture {arch:?}"),
    }
}

fn download(url: &str, destination: &Path) -> Result<()> {
    checked(
        "curl",
        [
            OsStr::new("-fL"),
            OsStr::new("--retry"),
            OsStr::new("3"),
            OsStr::new("--connect-timeout"),
            OsStr::new("10"),
            OsStr::new("--output"),
            destination.as_os_str(),
            OsStr::new(url),
        ],
    )
    .with_context(|| format!("failed to download {url}"))
}

fn verify_checksum(archive: &Path, sums: &Path, asset: &str) -> Result<()> {
    let sums_text = fs::read_to_string(sums).context("failed to read SHA256SUMS")?;
    let expected = sums_text
        .lines()
        .find_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == asset).then_some(hash)
        })
        .with_context(|| format!("SHA256SUMS does not contain {asset}"))?;

    if expected.len() != 64 || !expected.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("invalid SHA-256 value for {asset}");
    }

    let output = checked_output("sha256sum", [archive.as_os_str()])
        .context("failed to calculate SHA-256; sha256sum is required")?;
    let stdout = String::from_utf8(output.stdout).context("sha256sum output was not UTF-8")?;
    let actual = stdout
        .split_whitespace()
        .next()
        .context("sha256sum returned no digest")?;

    if !actual.eq_ignore_ascii_case(expected) {
        bail!("SHA-256 verification failed for {asset}");
    }
    Ok(())
}

fn verify_binary_version(candidate: &Path, tag: &str) -> Result<()> {
    let output = Command::new(candidate)
        .arg("version")
        .output()
        .context("downloaded binary could not be executed")?;
    if !output.status.success() {
        bail!("downloaded binary failed its version check");
    }

    let stdout = String::from_utf8(output.stdout).context("version output was not UTF-8")?;
    let expected = tag.trim_start_matches('v');
    if stdout.trim() != format!("chatgpt-bridge {expected}") {
        bail!(
            "downloaded binary version mismatch: expected {expected}, got {:?}",
            stdout.trim()
        );
    }
    Ok(())
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

fn checked_output<I, S>(program: &str, args: I) -> Result<Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program}"))?;
    ensure_success(program, output.status)?;
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

fn ensure_success(program: &str, status: ExitStatus) -> Result<()> {
    if status.success() {
        return Ok(());
    }

    match status.code() {
        Some(code) => bail!("{program} exited with status {code}"),
        None => bail!("{program} terminated by signal"),
    }
}

fn unique_suffix() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(test)]
mod tests {
    use super::{UpdateOptions, normalize_tag, parse_args};

    #[test]
    fn parses_update_options() {
        assert_eq!(parse_args(&[]).unwrap(), UpdateOptions::default());
        assert_eq!(
            parse_args(&["--check".to_owned()]).unwrap(),
            UpdateOptions {
                check: true,
                version: None,
            }
        );
        assert_eq!(
            parse_args(&["--version".to_owned(), "0.2.0".to_owned()]).unwrap(),
            UpdateOptions {
                check: false,
                version: Some("0.2.0".to_owned()),
            }
        );
    }

    #[test]
    fn normalizes_versions() {
        assert_eq!(normalize_tag("0.2.0").unwrap(), "v0.2.0");
        assert_eq!(normalize_tag("v0.2.0").unwrap(), "v0.2.0");
        assert_eq!(normalize_tag("1.0.0-rc.1").unwrap(), "v1.0.0-rc.1");
    }

    #[test]
    fn rejects_invalid_versions() {
        assert!(normalize_tag("").is_err());
        assert!(normalize_tag("../latest").is_err());
        assert!(normalize_tag("1.0.0 latest").is_err());
        assert!(parse_args(&["--nope".to_owned()]).is_err());
    }
}
