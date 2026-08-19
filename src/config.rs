use anyhow::{Context, Result, bail};
use std::{env, net::SocketAddr, path::PathBuf};

const DEFAULT_BIND: &str = "127.0.0.1:8787";
const DEFAULT_TIMEOUT_MS: u64 = 30_000;
const MAX_TIMEOUT_MS: u64 = 300_000;
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1_048_576;
const DEFAULT_MAX_FILE_BYTES: usize = 1_048_576;

#[derive(Debug, Clone)]
pub struct TlsConfig {
    pub cert: PathBuf,
    pub key: PathBuf,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub token: String,
    pub root: PathBuf,
    pub tls: Option<TlsConfig>,
    pub default_timeout_ms: u64,
    pub max_timeout_ms: u64,
    pub max_output_bytes: usize,
    pub max_file_bytes: usize,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let token = required("CHATGPT_BRIDGE_TOKEN")?;
        if token.len() < 32 {
            bail!("CHATGPT_BRIDGE_TOKEN must contain at least 32 characters");
        }

        let root_raw = required("CHATGPT_BRIDGE_ROOT")?;
        let root = PathBuf::from(&root_raw)
            .canonicalize()
            .with_context(|| format!("failed to resolve CHATGPT_BRIDGE_ROOT={root_raw:?}"))?;
        if !root.is_dir() {
            bail!("CHATGPT_BRIDGE_ROOT must point to an existing directory");
        }

        let bind_raw = env::var("CHATGPT_BRIDGE_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_owned());
        let bind = bind_raw
            .parse::<SocketAddr>()
            .with_context(|| format!("invalid CHATGPT_BRIDGE_BIND={bind_raw:?}"))?;

        let tls = match (
            optional("CHATGPT_BRIDGE_TLS_CERT"),
            optional("CHATGPT_BRIDGE_TLS_KEY"),
        ) {
            (None, None) => None,
            (Some(cert), Some(key)) => {
                let cert = PathBuf::from(&cert).canonicalize().with_context(|| {
                    format!("failed to resolve CHATGPT_BRIDGE_TLS_CERT={cert:?}")
                })?;
                let key = PathBuf::from(&key)
                    .canonicalize()
                    .with_context(|| format!("failed to resolve CHATGPT_BRIDGE_TLS_KEY={key:?}"))?;

                if !cert.is_file() || !key.is_file() {
                    bail!("TLS certificate and private key must both be regular files");
                }

                Some(TlsConfig { cert, key })
            }
            _ => bail!(
                "CHATGPT_BRIDGE_TLS_CERT and CHATGPT_BRIDGE_TLS_KEY must be configured together"
            ),
        };

        if !bind.ip().is_loopback() && tls.is_none() {
            bail!(
                "refusing to listen on a public interface without TLS; configure a certificate and key first"
            );
        }

        let default_timeout_ms =
            parse_u64("CHATGPT_BRIDGE_DEFAULT_TIMEOUT_MS", DEFAULT_TIMEOUT_MS)?;
        let max_timeout_ms = parse_u64("CHATGPT_BRIDGE_MAX_TIMEOUT_MS", MAX_TIMEOUT_MS)?;
        if default_timeout_ms == 0 || max_timeout_ms == 0 {
            bail!("command timeouts must be greater than zero");
        }
        if default_timeout_ms > max_timeout_ms {
            bail!("CHATGPT_BRIDGE_DEFAULT_TIMEOUT_MS cannot exceed CHATGPT_BRIDGE_MAX_TIMEOUT_MS");
        }

        let max_output_bytes =
            parse_usize("CHATGPT_BRIDGE_MAX_OUTPUT_BYTES", DEFAULT_MAX_OUTPUT_BYTES)?;
        let max_file_bytes = parse_usize("CHATGPT_BRIDGE_MAX_FILE_BYTES", DEFAULT_MAX_FILE_BYTES)?;
        if max_output_bytes == 0 || max_file_bytes == 0 {
            bail!("output and file byte limits must be greater than zero");
        }

        Ok(Self {
            bind,
            token,
            root,
            tls,
            default_timeout_ms,
            max_timeout_ms,
            max_output_bytes,
            max_file_bytes,
        })
    }
}

fn required(name: &str) -> Result<String> {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => bail!("{name} is required"),
    }
}

fn optional(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn parse_u64(name: &str, default: u64) -> Result<u64> {
    let raw = env::var(name).unwrap_or_else(|_| default.to_string());
    raw.parse::<u64>()
        .with_context(|| format!("invalid {name}={raw:?}"))
}

fn parse_usize(name: &str, default: usize) -> Result<usize> {
    let raw = env::var(name).unwrap_or_else(|_| default.to_string());
    raw.parse::<usize>()
        .with_context(|| format!("invalid {name}={raw:?}"))
}
