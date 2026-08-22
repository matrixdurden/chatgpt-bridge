use crate::{AppState, error::ApiError, workspace};
use axum::{Json, extract::State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{cmp::min, time::Instant};
use tokio::{fs, process::Command, time};

const MAX_COMMAND_BYTES: usize = 65_536;
const DEFAULT_LIST_LIMIT: usize = 200;
const MAX_LIST_LIMIT: usize = 1_000;

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "service": "chatgpt-bridge",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

#[derive(Serialize)]
pub struct InfoResponse {
    service: &'static str,
    version: &'static str,
    workspace_root: String,
    default_timeout_ms: u64,
    max_timeout_ms: u64,
    max_output_bytes: usize,
    max_file_bytes: usize,
    capabilities: Vec<&'static str>,
}

pub async fn info(State(state): State<AppState>) -> Json<InfoResponse> {
    Json(InfoResponse {
        service: "chatgpt-bridge",
        version: env!("CARGO_PKG_VERSION"),
        workspace_root: state.config.root.to_string_lossy().into_owned(),
        default_timeout_ms: state.config.default_timeout_ms,
        max_timeout_ms: state.config.max_timeout_ms,
        max_output_bytes: state.config.max_output_bytes,
        max_file_bytes: state.config.max_file_bytes,
        capabilities: vec![
            "exec",
            "file_read",
            "file_write",
            "file_list",
            "checkpoints",
        ],
    })
}

#[derive(Deserialize)]
pub struct ExecRequest {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct ExecResponse {
    success: bool,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    duration_ms: u128,
}

pub async fn exec(
    State(state): State<AppState>,
    Json(request): Json<ExecRequest>,
) -> Result<Json<ExecResponse>, ApiError> {
    if request.command.trim().is_empty() {
        return Err(ApiError::bad_request("command cannot be empty"));
    }
    if request.command.len() > MAX_COMMAND_BYTES {
        return Err(ApiError::payload_too_large(format!(
            "command exceeds {MAX_COMMAND_BYTES} bytes"
        )));
    }

    let _change_guard = state.change_lock.lock().await;
    let cwd_relative = request.cwd.as_deref().unwrap_or("");
    let cwd = workspace::resolve_existing(&state.config.root, cwd_relative).await?;
    let cwd_metadata = fs::metadata(&cwd)
        .await
        .map_err(|error| ApiError::internal(format!("failed to inspect command cwd: {error}")))?;
    if !cwd_metadata.is_dir() {
        return Err(ApiError::bad_request("cwd must be a directory"));
    }

    let timeout_ms = request
        .timeout_ms
        .unwrap_or(state.config.default_timeout_ms);
    if timeout_ms == 0 || timeout_ms > state.config.max_timeout_ms {
        return Err(ApiError::bad_request(format!(
            "timeout_ms must be between 1 and {}",
            state.config.max_timeout_ms
        )));
    }

    let started = Instant::now();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-lc")
        .arg(&request.command)
        .current_dir(&cwd)
        .kill_on_drop(true);

    let output =
        match time::timeout(time::Duration::from_millis(timeout_ms), command.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                return Err(ApiError::internal(format!(
                    "failed to execute command: {error}"
                )));
            }
            Err(_) => {
                return Err(ApiError::timeout(format!(
                    "command exceeded timeout of {timeout_ms} ms"
                )));
            }
        };

    let (stdout, stdout_truncated) = truncate_output(&output.stdout, state.config.max_output_bytes);
    let (stderr, stderr_truncated) = truncate_output(&output.stderr, state.config.max_output_bytes);

    Ok(Json(ExecResponse {
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout,
        stderr,
        stdout_truncated,
        stderr_truncated,
        duration_ms: started.elapsed().as_millis(),
    }))
}

#[derive(Deserialize)]
pub struct ReadFileRequest {
    path: String,
}

#[derive(Serialize)]
pub struct ReadFileResponse {
    path: String,
    content: String,
    bytes: usize,
}

pub async fn read_file(
    State(state): State<AppState>,
    Json(request): Json<ReadFileRequest>,
) -> Result<Json<ReadFileResponse>, ApiError> {
    let path = workspace::resolve_existing(&state.config.root, &request.path).await?;
    let metadata = fs::metadata(&path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to inspect file: {error}")))?;

    if !metadata.is_file() {
        return Err(ApiError::bad_request("path must point to a regular file"));
    }
    if metadata.len() > state.config.max_file_bytes as u64 {
        return Err(ApiError::payload_too_large(format!(
            "file exceeds {} bytes",
            state.config.max_file_bytes
        )));
    }

    let bytes = fs::read(&path)
        .await
        .map_err(|error| ApiError::internal(format!("failed to read file: {error}")))?;
    if bytes.len() > state.config.max_file_bytes {
        return Err(ApiError::payload_too_large(format!(
            "file exceeds {} bytes",
            state.config.max_file_bytes
        )));
    }

    let byte_count = bytes.len();
    let content = String::from_utf8(bytes)
        .map_err(|_| ApiError::bad_request("file is not valid UTF-8 text"))?;

    Ok(Json(ReadFileResponse {
        path: request.path,
        content,
        bytes: byte_count,
    }))
}

#[derive(Deserialize)]
pub struct WriteFileRequest {
    path: String,
    content: String,
    #[serde(default)]
    create_parents: bool,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Serialize)]
pub struct WriteFileResponse {
    path: String,
    bytes: usize,
    created: bool,
}

pub async fn write_file(
    State(state): State<AppState>,
    Json(request): Json<WriteFileRequest>,
) -> Result<Json<WriteFileResponse>, ApiError> {
    let _change_guard = state.change_lock.lock().await;
    let byte_count = request.content.len();
    if byte_count > state.config.max_file_bytes {
        return Err(ApiError::payload_too_large(format!(
            "content exceeds {} bytes",
            state.config.max_file_bytes
        )));
    }

    let target =
        workspace::resolve_write_target(&state.config.root, &request.path, request.create_parents)
            .await?;

    let existed = match fs::symlink_metadata(&target).await {
        Ok(metadata) => {
            if metadata.is_dir() {
                return Err(ApiError::bad_request("path points to a directory"));
            }
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(ApiError::internal(format!(
                "failed to inspect target file: {error}"
            )));
        }
    };

    if existed && !request.overwrite {
        return Err(ApiError::conflict(
            "file already exists; set overwrite=true to replace it",
        ));
    }

    fs::write(&target, request.content.as_bytes())
        .await
        .map_err(|error| ApiError::internal(format!("failed to write file: {error}")))?;

    Ok(Json(WriteFileResponse {
        path: request.path,
        bytes: byte_count,
        created: !existed,
    }))
}

#[derive(Deserialize)]
pub struct ListFilesRequest {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
pub struct ListFilesResponse {
    path: String,
    entries: Vec<DirectoryEntry>,
    truncated: bool,
}

#[derive(Serialize)]
pub struct DirectoryEntry {
    name: String,
    path: String,
    kind: &'static str,
    size: Option<u64>,
}

pub async fn list_files(
    State(state): State<AppState>,
    Json(request): Json<ListFilesRequest>,
) -> Result<Json<ListFilesResponse>, ApiError> {
    let relative = request.path.unwrap_or_default();
    let directory = workspace::resolve_existing(&state.config.root, &relative).await?;
    let metadata = fs::metadata(&directory)
        .await
        .map_err(|error| ApiError::internal(format!("failed to inspect directory: {error}")))?;
    if !metadata.is_dir() {
        return Err(ApiError::bad_request("path must point to a directory"));
    }

    let limit = request.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    if limit == 0 || limit > MAX_LIST_LIMIT {
        return Err(ApiError::bad_request(format!(
            "limit must be between 1 and {MAX_LIST_LIMIT}"
        )));
    }

    let mut read_dir = fs::read_dir(&directory)
        .await
        .map_err(|error| ApiError::internal(format!("failed to list directory: {error}")))?;
    let mut entries = Vec::with_capacity(min(limit, 128));
    let mut truncated = false;

    while let Some(entry) = read_dir
        .next_entry()
        .await
        .map_err(|error| ApiError::internal(format!("failed while listing directory: {error}")))?
    {
        if entries.len() >= limit {
            truncated = true;
            break;
        }

        let file_type = entry.file_type().await.map_err(|error| {
            ApiError::internal(format!("failed to inspect directory entry: {error}"))
        })?;

        let (kind, size) = if file_type.is_file() {
            let size = entry.metadata().await.ok().map(|metadata| metadata.len());
            ("file", size)
        } else if file_type.is_dir() {
            ("directory", None)
        } else if file_type.is_symlink() {
            ("symlink", None)
        } else {
            ("other", None)
        };

        let entry_path = entry.path();
        let relative_path = entry_path
            .strip_prefix(&state.config.root)
            .unwrap_or(&entry_path)
            .to_string_lossy()
            .into_owned();

        entries.push(DirectoryEntry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: relative_path,
            kind,
            size,
        });
    }

    entries.sort_by(|left, right| left.name.cmp(&right.name));

    Ok(Json(ListFilesResponse {
        path: relative,
        entries,
        truncated,
    }))
}

fn truncate_output(bytes: &[u8], max_bytes: usize) -> (String, bool) {
    let truncated = bytes.len() > max_bytes;
    let slice = &bytes[..min(bytes.len(), max_bytes)];
    (String::from_utf8_lossy(slice).into_owned(), truncated)
}

#[cfg(test)]
mod tests {
    use super::truncate_output;

    #[test]
    fn truncates_output_at_configured_limit() {
        let (value, truncated) = truncate_output(b"abcdef", 3);
        assert_eq!(value, "abc");
        assert!(truncated);

        let (value, truncated) = truncate_output(b"abc", 3);
        assert_eq!(value, "abc");
        assert!(!truncated);
    }
}

#[derive(Deserialize)]
pub struct BeginChangeRequest {
    #[serde(default)]
    cwd: Option<String>,
}

pub async fn begin_change(
    State(state): State<AppState>,
    Json(request): Json<BeginChangeRequest>,
) -> Result<Json<crate::checkpoint::BeginResult>, ApiError> {
    let scope_relative = request.cwd.as_deref().unwrap_or("");
    let scope = workspace::resolve_existing(&state.config.root, scope_relative).await?;
    let metadata = fs::metadata(&scope).await.map_err(|error| {
        ApiError::internal(format!("failed to inspect checkpoint scope: {error}"))
    })?;
    if !metadata.is_dir() {
        return Err(ApiError::bad_request("checkpoint cwd must be a directory"));
    }

    let _guard = state.change_lock.lock().await;
    let store = std::sync::Arc::clone(&state.checkpoints);
    let result = tokio::task::spawn_blocking(move || store.begin(&scope))
        .await
        .map_err(|error| ApiError::internal(format!("checkpoint task failed: {error}")))??;
    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct FinishChangeRequest {
    transaction_id: String,
}

pub async fn finish_change(
    State(state): State<AppState>,
    Json(request): Json<FinishChangeRequest>,
) -> Result<Json<crate::checkpoint::FinishResult>, ApiError> {
    let _guard = state.change_lock.lock().await;
    let store = std::sync::Arc::clone(&state.checkpoints);
    let transaction_id = request.transaction_id;
    let result = tokio::task::spawn_blocking(move || store.finish(&transaction_id))
        .await
        .map_err(|error| ApiError::internal(format!("checkpoint task failed: {error}")))??;
    Ok(Json(result))
}

#[derive(Deserialize)]
pub struct RestoreCheckpointRequest {
    id: String,
    #[serde(default)]
    force: bool,
}

pub async fn restore_checkpoint(
    State(state): State<AppState>,
    Json(request): Json<RestoreCheckpointRequest>,
) -> Result<Json<crate::checkpoint::RestoreResult>, ApiError> {
    let _guard = state.change_lock.lock().await;
    let store = std::sync::Arc::clone(&state.checkpoints);
    let id = request.id;
    let force = request.force;
    let result = tokio::task::spawn_blocking(move || store.restore(&id, force))
        .await
        .map_err(|error| ApiError::internal(format!("checkpoint task failed: {error}")))??;
    Ok(Json(result))
}

pub async fn undo_checkpoint(
    State(state): State<AppState>,
    Json(request): Json<RestoreCheckpointRequest>,
) -> Result<Json<crate::checkpoint::RestoreResult>, ApiError> {
    let _guard = state.change_lock.lock().await;
    let store = std::sync::Arc::clone(&state.checkpoints);
    let id = request.id;
    let force = request.force;
    let result = tokio::task::spawn_blocking(move || store.undo(&id, force))
        .await
        .map_err(|error| ApiError::internal(format!("checkpoint task failed: {error}")))??;
    Ok(Json(result))
}

pub async fn list_checkpoints(
    State(state): State<AppState>,
) -> Result<Json<Vec<crate::checkpoint::CheckpointInfo>>, ApiError> {
    let store = std::sync::Arc::clone(&state.checkpoints);
    let result = tokio::task::spawn_blocking(move || store.list())
        .await
        .map_err(|error| ApiError::internal(format!("checkpoint task failed: {error}")))??;
    Ok(Json(result))
}
