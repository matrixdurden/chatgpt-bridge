use crate::error::ApiError;
use std::{
    io::ErrorKind,
    path::{Component, Path, PathBuf},
};
use tokio::fs;

pub async fn resolve_existing(root: &Path, relative: &str) -> Result<PathBuf, ApiError> {
    let clean = sanitize_relative(relative)?;
    let candidate = root.join(clean);
    let canonical = fs::canonicalize(&candidate).await.map_err(|error| {
        if error.kind() == ErrorKind::NotFound {
            ApiError::not_found(format!("path does not exist: {relative:?}"))
        } else {
            ApiError::internal(format!("failed to resolve path: {error}"))
        }
    })?;

    ensure_inside(root, &canonical)?;
    Ok(canonical)
}

pub async fn resolve_write_target(
    root: &Path,
    relative: &str,
    create_parents: bool,
) -> Result<PathBuf, ApiError> {
    let clean = sanitize_relative(relative)?;
    if clean.as_os_str().is_empty() {
        return Err(ApiError::bad_request("file path cannot be empty"));
    }

    let candidate = root.join(clean);

    match fs::symlink_metadata(&candidate).await {
        Ok(_) => {
            let canonical = fs::canonicalize(&candidate).await.map_err(|error| {
                ApiError::internal(format!("failed to resolve file path: {error}"))
            })?;
            ensure_inside(root, &canonical)?;
            return Ok(canonical);
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => {
            return Err(ApiError::internal(format!(
                "failed to inspect file path: {error}"
            )));
        }
    }

    let parent = candidate
        .parent()
        .ok_or_else(|| ApiError::bad_request("file path has no parent directory"))?;

    let mut existing_ancestor = parent.to_path_buf();
    loop {
        match fs::symlink_metadata(&existing_ancestor).await {
            Ok(_) => break,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let Some(next) = existing_ancestor.parent() else {
                    return Err(ApiError::forbidden("path escapes the configured workspace"));
                };
                existing_ancestor = next.to_path_buf();
            }
            Err(error) => {
                return Err(ApiError::internal(format!(
                    "failed to inspect parent directory: {error}"
                )));
            }
        }
    }

    let canonical_ancestor = fs::canonicalize(&existing_ancestor)
        .await
        .map_err(|error| {
            ApiError::internal(format!("failed to resolve parent directory: {error}"))
        })?;
    ensure_inside(root, &canonical_ancestor)?;

    if create_parents {
        fs::create_dir_all(parent).await.map_err(|error| {
            ApiError::internal(format!("failed to create parent directories: {error}"))
        })?;
    } else if fs::symlink_metadata(parent).await.is_err() {
        return Err(ApiError::not_found("parent directory does not exist"));
    }

    let canonical_parent = fs::canonicalize(parent).await.map_err(|error| {
        ApiError::internal(format!("failed to resolve parent directory: {error}"))
    })?;
    ensure_inside(root, &canonical_parent)?;

    let file_name = candidate
        .file_name()
        .ok_or_else(|| ApiError::bad_request("invalid file name"))?;

    Ok(canonical_parent.join(file_name))
}

fn sanitize_relative(relative: &str) -> Result<PathBuf, ApiError> {
    let path = Path::new(relative);
    if path.is_absolute() {
        return Err(ApiError::forbidden("absolute paths are not allowed"));
    }

    let mut clean = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => {
                return Err(ApiError::forbidden(
                    "path traversal and rooted paths are not allowed",
                ));
            }
        }
    }

    Ok(clean)
}

fn ensure_inside(root: &Path, resolved: &Path) -> Result<(), ApiError> {
    if resolved.starts_with(root) {
        Ok(())
    } else {
        Err(ApiError::forbidden(
            "resolved path escapes the configured workspace",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_relative;
    use std::path::PathBuf;

    #[test]
    fn accepts_clean_relative_paths() {
        assert_eq!(
            sanitize_relative("project/src/main.rs").unwrap(),
            PathBuf::from("project/src/main.rs")
        );
        assert_eq!(
            sanitize_relative("./project").unwrap(),
            PathBuf::from("project")
        );
    }

    #[test]
    fn rejects_parent_traversal_and_absolute_paths() {
        assert!(sanitize_relative("../secret").is_err());
        assert!(sanitize_relative("project/../../secret").is_err());
        assert!(sanitize_relative("/etc/passwd").is_err());
    }
}
