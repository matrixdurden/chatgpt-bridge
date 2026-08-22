use crate::error::ApiError;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap},
    env,
    ffi::OsStr,
    fs,
    io::{self, ErrorKind, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const VCS_METADATA_DIRS: [&str; 3] = [".git", ".hg", ".svn"];
const GENERATED_DIRS: [&str; 13] = [
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".cache",
    "dist",
    "build",
    ".next",
    ".nuxt",
    "coverage",
];

#[derive(Debug, Clone)]
pub struct CheckpointStore {
    workspace_root: PathBuf,
    state_root: PathBuf,
    store_root: PathBuf,
    include_generated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Entry {
    File {
        object: String,
        mode: u32,
        size: u64,
    },
    Directory {
        mode: u32,
    },
    Symlink {
        target: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Manifest {
    id: String,
    scope: String,
    parent: Option<String>,
    created_at_ms: u128,
    visible: bool,
    #[serde(default)]
    summary: ChangeSummary,
    entries: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Transaction {
    id: String,
    scope: String,
    base: String,
    created_at_ms: u128,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Refs {
    heads: HashMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ChangeSummary {
    pub created: usize,
    pub modified: usize,
    pub deleted: usize,
}

impl ChangeSummary {
    pub fn total(&self) -> usize {
        self.created + self.modified + self.deleted
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct BeginResult {
    pub transaction_id: String,
    pub scope: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FinishResult {
    pub changed: bool,
    pub change_id: Option<String>,
    pub summary: ChangeSummary,
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreResult {
    pub restored_to: String,
    pub scope: String,
    pub safety_checkpoint_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckpointInfo {
    pub id: String,
    pub scope: String,
    pub parent: Option<String>,
    pub created_at_ms: u128,
    pub summary: ChangeSummary,
}

impl CheckpointStore {
    pub fn new(workspace_root: &Path) -> Result<Self, ApiError> {
        Self::with_state_root(
            workspace_root,
            state_root()?,
            env_bool("CHATGPT_BRIDGE_CHECKPOINT_INCLUDE_GENERATED", false)?,
        )
    }

    fn with_state_root(
        workspace_root: &Path,
        state_root: PathBuf,
        include_generated: bool,
    ) -> Result<Self, ApiError> {
        create_private_dir_all(&state_root)?;
        let state_root = state_root
            .canonicalize()
            .map_err(io_internal("failed to resolve checkpoint state directory"))?;

        let workspace_key = digest_hex(workspace_root.as_os_str().as_encoded_bytes());
        let store_root = state_root.join(&workspace_key[..24]);
        create_private_dir_all(&store_root)?;
        create_private_dir_all(&store_root.join("objects"))?;
        create_private_dir_all(&store_root.join("manifests"))?;
        create_private_dir_all(&store_root.join("transactions"))?;

        Ok(Self {
            workspace_root: workspace_root.to_path_buf(),
            state_root,
            store_root,
            include_generated,
        })
    }

    pub fn begin(&self, scope_root: &Path) -> Result<BeginResult, ApiError> {
        let scope = self.scope_string(scope_root)?;
        let current = self.snapshot(scope_root)?;
        let mut refs = self.read_refs()?;

        let base = match refs.heads.get(&scope).cloned() {
            Some(head_id) => {
                let head = self.read_manifest(&head_id)?;
                if head.entries == current {
                    head_id
                } else {
                    let id = self.unique_id("base")?;
                    let manifest = Manifest {
                        id: id.clone(),
                        scope: scope.clone(),
                        parent: Some(head_id),
                        created_at_ms: now_ms(),
                        visible: false,
                        summary: ChangeSummary::default(),
                        entries: current,
                    };
                    self.write_manifest(&manifest)?;
                    refs.heads.insert(scope.clone(), id.clone());
                    self.write_refs(&refs)?;
                    id
                }
            }
            None => {
                let id = self.unique_id("base")?;
                let manifest = Manifest {
                    id: id.clone(),
                    scope: scope.clone(),
                    parent: None,
                    created_at_ms: now_ms(),
                    visible: false,
                    summary: ChangeSummary::default(),
                    entries: current,
                };
                self.write_manifest(&manifest)?;
                refs.heads.insert(scope.clone(), id.clone());
                self.write_refs(&refs)?;
                id
            }
        };

        let transaction_id = self.unique_transaction_id()?;
        let transaction = Transaction {
            id: transaction_id.clone(),
            scope: scope.clone(),
            base,
            created_at_ms: now_ms(),
        };
        self.write_json_atomic(&self.transaction_path(&transaction_id), &transaction)?;

        Ok(BeginResult {
            transaction_id,
            scope,
        })
    }

    pub fn finish(&self, transaction_id: &str) -> Result<FinishResult, ApiError> {
        let transaction = self.read_transaction(transaction_id)?;
        let base = self.read_manifest(&transaction.base)?;
        let refs = self.read_refs()?;
        if refs.heads.get(&transaction.scope) != Some(&transaction.base) {
            return Err(ApiError::conflict(
                "checkpoint transaction is stale because the workspace history changed",
            ));
        }

        let scope_root = self.scope_root(&transaction.scope)?;
        let current = self.snapshot(&scope_root)?;
        let summary = diff_summary(&base.entries, &current);

        if summary.total() == 0 {
            self.remove_transaction(transaction_id)?;
            return Ok(FinishResult {
                changed: false,
                change_id: None,
                summary,
            });
        }

        let id = self.unique_id("chg")?;
        let manifest = Manifest {
            id: id.clone(),
            scope: transaction.scope.clone(),
            parent: Some(base.id),
            created_at_ms: now_ms(),
            visible: true,
            summary: summary.clone(),
            entries: current,
        };
        self.write_manifest(&manifest)?;

        let mut refs = self.read_refs()?;
        refs.heads.insert(transaction.scope, id.clone());
        self.write_refs(&refs)?;
        self.remove_transaction(transaction_id)?;

        Ok(FinishResult {
            changed: true,
            change_id: Some(id),
            summary,
        })
    }

    pub fn restore(&self, requested_id: &str, force: bool) -> Result<RestoreResult, ApiError> {
        let target = self.resolve_manifest(requested_id)?;
        self.restore_manifest(target, force)
    }

    pub fn undo(&self, requested_id: &str, force: bool) -> Result<RestoreResult, ApiError> {
        let checkpoint = self.resolve_manifest(requested_id)?;
        let parent = checkpoint
            .parent
            .clone()
            .ok_or_else(|| ApiError::conflict("checkpoint has no parent state to restore"))?;
        let target = self.read_manifest(&parent)?;
        self.restore_manifest(target, force)
    }

    pub fn list(&self) -> Result<Vec<CheckpointInfo>, ApiError> {
        let mut checkpoints = Vec::new();
        for entry in fs::read_dir(self.store_root.join("manifests"))
            .map_err(io_internal("failed to list checkpoint manifests"))?
        {
            let entry = entry.map_err(io_internal("failed to list checkpoint manifests"))?;
            if !entry
                .file_type()
                .map_err(io_internal("failed to inspect checkpoint manifest"))?
                .is_file()
            {
                continue;
            }
            let bytes = fs::read(entry.path())
                .map_err(io_internal("failed to read checkpoint manifest"))?;
            let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|error| {
                ApiError::internal(format!("invalid checkpoint manifest: {error}"))
            })?;
            if manifest.visible {
                checkpoints.push(CheckpointInfo {
                    id: manifest.id,
                    scope: manifest.scope,
                    parent: manifest.parent,
                    created_at_ms: manifest.created_at_ms,
                    summary: manifest.summary,
                });
            }
        }
        checkpoints.sort_by_key(|item| std::cmp::Reverse(item.created_at_ms));
        Ok(checkpoints)
    }

    fn restore_manifest(&self, target: Manifest, force: bool) -> Result<RestoreResult, ApiError> {
        let scope_root = self.scope_root(&target.scope)?;
        let current = self.snapshot(&scope_root)?;
        let refs = self.read_refs()?;
        let current_head = refs.heads.get(&target.scope).cloned();

        let diverged = match &current_head {
            Some(head) => self.read_manifest(head)?.entries != current,
            None => !current.is_empty(),
        };

        if diverged && !force {
            return Err(ApiError::conflict(
                "workspace has changes outside checkpoint history; retry with force=true to preserve them in a safety checkpoint before restoring",
            ));
        }

        let safety_checkpoint_id = if diverged {
            let id = self.unique_id("safe")?;
            let safety = Manifest {
                id: id.clone(),
                scope: target.scope.clone(),
                parent: current_head.clone(),
                created_at_ms: now_ms(),
                visible: false,
                summary: ChangeSummary::default(),
                entries: current.clone(),
            };
            self.write_manifest(&safety)?;
            Some(id)
        } else {
            None
        };

        if let Err(error) = self.apply_manifest(&scope_root, &current, &target.entries) {
            let _ = self.apply_manifest(
                &scope_root,
                &self.snapshot(&scope_root).unwrap_or_default(),
                &current,
            );
            return Err(error);
        }

        let mut refs = self.read_refs()?;
        refs.heads.insert(target.scope.clone(), target.id.clone());
        self.write_refs(&refs)?;

        Ok(RestoreResult {
            restored_to: target.id,
            scope: target.scope,
            safety_checkpoint_id,
        })
    }

    fn snapshot(&self, scope_root: &Path) -> Result<BTreeMap<String, Entry>, ApiError> {
        let mut entries = BTreeMap::new();
        self.snapshot_dir(scope_root, scope_root, &mut entries)?;
        Ok(entries)
    }

    fn snapshot_dir(
        &self,
        scope_root: &Path,
        directory: &Path,
        entries: &mut BTreeMap<String, Entry>,
    ) -> Result<(), ApiError> {
        let mut children = fs::read_dir(directory)
            .map_err(io_internal("failed to read workspace during checkpoint"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(io_internal("failed to read workspace during checkpoint"))?;
        children.sort_by_key(|entry| entry.file_name());

        for child in children {
            let path = child.path();
            if path.starts_with(&self.state_root) {
                continue;
            }
            let name = child.file_name();
            if is_vcs_metadata(name.as_os_str()) {
                continue;
            }
            if !self.include_generated && is_generated_dir(name.as_os_str()) {
                continue;
            }

            let metadata = fs::symlink_metadata(&path)
                .map_err(io_internal("failed to inspect workspace during checkpoint"))?;
            let relative = relative_utf8(scope_root, &path)?;

            if metadata.file_type().is_symlink() {
                let target = fs::read_link(&path)
                    .map_err(io_internal("failed to read symlink during checkpoint"))?;
                let target = target.to_str().ok_or_else(|| {
                    ApiError::bad_request(format!("symlink target is not valid UTF-8: {relative}"))
                })?;
                entries.insert(
                    relative,
                    Entry::Symlink {
                        target: target.to_owned(),
                    },
                );
            } else if metadata.is_dir() {
                entries.insert(
                    relative,
                    Entry::Directory {
                        mode: metadata.permissions().mode(),
                    },
                );
                self.snapshot_dir(scope_root, &path, entries)?;
            } else if metadata.is_file() {
                let bytes = fs::read(&path)
                    .map_err(io_internal("failed to read file during checkpoint"))?;
                let object = digest_hex(&bytes);
                self.store_object(&object, &bytes)?;
                entries.insert(
                    relative,
                    Entry::File {
                        object,
                        mode: metadata.permissions().mode(),
                        size: metadata.len(),
                    },
                );
            }
        }
        Ok(())
    }

    fn apply_manifest(
        &self,
        scope_root: &Path,
        current: &BTreeMap<String, Entry>,
        target: &BTreeMap<String, Entry>,
    ) -> Result<(), ApiError> {
        let mut removals = current
            .iter()
            .filter(|(path, current_entry)| {
                target
                    .get(*path)
                    .is_none_or(|target_entry| !same_kind(current_entry, target_entry))
            })
            .map(|(path, entry)| (path.clone(), entry.clone()))
            .collect::<Vec<_>>();
        removals.sort_by_key(|(path, _)| std::cmp::Reverse(path_depth(path)));

        for (relative, entry) in removals {
            let path = scope_root.join(&relative);
            match entry {
                Entry::Directory { .. } => match fs::remove_dir(&path) {
                    Ok(()) => {}
                    Err(error)
                        if matches!(
                            error.kind(),
                            ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                        ) => {}
                    Err(error) => {
                        return Err(ApiError::internal(format!(
                            "failed to remove directory {relative:?}: {error}"
                        )));
                    }
                },
                Entry::File { .. } | Entry::Symlink { .. } => match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(ApiError::internal(format!(
                            "failed to remove path {relative:?}: {error}"
                        )));
                    }
                },
            }
        }

        let mut directories = target
            .iter()
            .filter_map(|(path, entry)| match entry {
                Entry::Directory { mode } => Some((path.clone(), *mode)),
                _ => None,
            })
            .collect::<Vec<_>>();
        directories.sort_by_key(|(path, _)| path_depth(path));

        for (relative, mode) in &directories {
            let path = scope_root.join(relative);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
                Ok(_) => {
                    remove_non_directory(&path, relative)?;
                    fs::create_dir(&path)
                        .map_err(io_internal("failed to create directory during restore"))?;
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    fs::create_dir(&path)
                        .map_err(io_internal("failed to create directory during restore"))?;
                }
                Err(error) => {
                    return Err(ApiError::internal(format!(
                        "failed to inspect {relative:?}: {error}"
                    )));
                }
            }
            fs::set_permissions(&path, fs::Permissions::from_mode(*mode | 0o700)).map_err(
                io_internal("failed to prepare directory permissions during restore"),
            )?;
        }

        for (relative, entry) in target {
            let path = scope_root.join(relative);
            match entry {
                Entry::File { object, mode, .. } => {
                    if matches!(fs::symlink_metadata(&path), Ok(metadata) if metadata.is_dir()) {
                        fs::remove_dir(&path).map_err(|error| {
                            ApiError::conflict(format!(
                                "cannot replace non-empty directory {relative:?}: {error}"
                            ))
                        })?;
                    }
                    let bytes = self.read_object(object)?;
                    write_atomic(&path, &bytes)?;
                    fs::set_permissions(&path, fs::Permissions::from_mode(*mode))
                        .map_err(io_internal("failed to restore file permissions"))?;
                }
                Entry::Symlink { target } => {
                    match fs::symlink_metadata(&path) {
                        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                            fs::remove_dir(&path).map_err(|error| {
                                ApiError::conflict(format!(
                                    "cannot replace non-empty directory {relative:?}: {error}"
                                ))
                            })?;
                        }
                        Ok(_) => {
                            fs::remove_file(&path)
                                .map_err(io_internal("failed to replace symlink during restore"))?;
                        }
                        Err(error) if error.kind() == ErrorKind::NotFound => {}
                        Err(error) => {
                            return Err(ApiError::internal(format!(
                                "failed to inspect {relative:?}: {error}"
                            )));
                        }
                    }
                    symlink(target, &path).map_err(io_internal("failed to restore symlink"))?;
                }
                Entry::Directory { .. } => {}
            }
        }

        directories.sort_by_key(|(path, _)| std::cmp::Reverse(path_depth(path)));
        for (relative, mode) in directories {
            let path = scope_root.join(relative);
            fs::set_permissions(&path, fs::Permissions::from_mode(mode))
                .map_err(io_internal("failed to restore directory permissions"))?;
        }

        Ok(())
    }

    fn scope_string(&self, scope_root: &Path) -> Result<String, ApiError> {
        let relative = scope_root.strip_prefix(&self.workspace_root).map_err(|_| {
            ApiError::forbidden("checkpoint scope escapes the configured workspace")
        })?;
        relative
            .to_str()
            .map(str::to_owned)
            .ok_or_else(|| ApiError::bad_request("checkpoint scope path must be valid UTF-8"))
    }

    fn scope_root(&self, scope: &str) -> Result<PathBuf, ApiError> {
        let candidate = self.workspace_root.join(scope);
        let canonical = candidate.canonicalize().map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                ApiError::not_found(format!("checkpoint scope no longer exists: {scope:?}"))
            } else {
                ApiError::internal(format!("failed to resolve checkpoint scope: {error}"))
            }
        })?;
        if !canonical.starts_with(&self.workspace_root) || !canonical.is_dir() {
            return Err(ApiError::forbidden(
                "checkpoint scope is no longer a safe workspace directory",
            ));
        }
        Ok(canonical)
    }

    fn store_object(&self, hash: &str, bytes: &[u8]) -> Result<(), ApiError> {
        let directory = self.store_root.join("objects").join(&hash[..2]);
        create_private_dir_all(&directory)?;
        let path = directory.join(hash);
        if path.exists() {
            return Ok(());
        }
        write_atomic(&path, bytes)
    }

    fn read_object(&self, hash: &str) -> Result<Vec<u8>, ApiError> {
        fs::read(self.store_root.join("objects").join(&hash[..2]).join(hash))
            .map_err(io_internal("failed to read checkpoint object"))
    }

    fn manifest_path(&self, id: &str) -> PathBuf {
        self.store_root.join("manifests").join(format!("{id}.json"))
    }

    fn transaction_path(&self, id: &str) -> PathBuf {
        self.store_root
            .join("transactions")
            .join(format!("{id}.json"))
    }

    fn write_manifest(&self, manifest: &Manifest) -> Result<(), ApiError> {
        self.write_json_atomic(&self.manifest_path(&manifest.id), manifest)
    }

    fn read_manifest(&self, id: &str) -> Result<Manifest, ApiError> {
        let path = self.manifest_path(id);
        let bytes = fs::read(&path).map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                ApiError::not_found(format!("checkpoint not found: {id}"))
            } else {
                ApiError::internal(format!("failed to read checkpoint: {error}"))
            }
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::internal(format!("invalid checkpoint manifest: {error}")))
    }

    fn resolve_manifest(&self, requested: &str) -> Result<Manifest, ApiError> {
        if let Ok(manifest) = self.read_manifest(requested) {
            return Ok(manifest);
        }

        let needle = requested.trim().to_ascii_uppercase();
        let mut matches = Vec::new();
        for entry in fs::read_dir(self.store_root.join("manifests"))
            .map_err(io_internal("failed to search checkpoints"))?
        {
            let entry = entry.map_err(io_internal("failed to search checkpoints"))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            let upper = id.to_ascii_uppercase();
            if upper == needle || upper.ends_with(&format!("-{needle}")) {
                matches.push(id.to_owned());
            }
        }
        match matches.as_slice() {
            [id] => self.read_manifest(id),
            [] => Err(ApiError::not_found(format!(
                "checkpoint not found: {requested}"
            ))),
            _ => Err(ApiError::conflict(format!(
                "checkpoint id is ambiguous: {requested}"
            ))),
        }
    }

    fn read_transaction(&self, id: &str) -> Result<Transaction, ApiError> {
        let bytes = fs::read(self.transaction_path(id)).map_err(|error| {
            if error.kind() == ErrorKind::NotFound {
                ApiError::not_found(format!("checkpoint transaction not found: {id}"))
            } else {
                ApiError::internal(format!("failed to read checkpoint transaction: {error}"))
            }
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|error| ApiError::internal(format!("invalid checkpoint transaction: {error}")))
    }

    fn remove_transaction(&self, id: &str) -> Result<(), ApiError> {
        match fs::remove_file(self.transaction_path(id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ApiError::internal(format!(
                "failed to remove checkpoint transaction: {error}"
            ))),
        }
    }

    fn read_refs(&self) -> Result<Refs, ApiError> {
        let path = self.store_root.join("refs.json");
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|error| ApiError::internal(format!("invalid checkpoint refs: {error}"))),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(Refs::default()),
            Err(error) => Err(ApiError::internal(format!(
                "failed to read checkpoint refs: {error}"
            ))),
        }
    }

    fn write_refs(&self, refs: &Refs) -> Result<(), ApiError> {
        self.write_json_atomic(&self.store_root.join("refs.json"), refs)
    }

    fn write_json_atomic<T: Serialize>(&self, path: &Path, value: &T) -> Result<(), ApiError> {
        let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
            ApiError::internal(format!("failed to serialize checkpoint state: {error}"))
        })?;
        write_atomic(path, &bytes)
    }

    fn unique_id(&self, prefix: &str) -> Result<String, ApiError> {
        for salt in 0..1000u32 {
            let seed = format!("{}:{}:{}:{}", now_ms(), std::process::id(), prefix, salt);
            let digest = digest_hex(seed.as_bytes());
            let id = format!("{prefix}-{}", digest[..6].to_ascii_uppercase());
            if !self.manifest_path(&id).exists() {
                return Ok(id);
            }
        }
        Err(ApiError::internal("failed to allocate checkpoint id"))
    }

    fn unique_transaction_id(&self) -> Result<String, ApiError> {
        for salt in 0..1000u32 {
            let seed = format!("{}:{}:tx:{}", now_ms(), std::process::id(), salt);
            let digest = digest_hex(seed.as_bytes());
            let id = format!("tx-{}", &digest[..10]);
            if !self.transaction_path(&id).exists() {
                return Ok(id);
            }
        }
        Err(ApiError::internal(
            "failed to allocate checkpoint transaction id",
        ))
    }
}

fn state_root() -> Result<PathBuf, ApiError> {
    if let Some(path) = env::var_os("CHATGPT_BRIDGE_STATE_DIR") {
        return Ok(PathBuf::from(path).join("checkpoints"));
    }
    if let Some(path) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("chatgpt-bridge/checkpoints"));
    }
    let home = env::var_os("HOME")
        .ok_or_else(|| ApiError::internal("HOME is not set; configure CHATGPT_BRIDGE_STATE_DIR"))?;
    Ok(PathBuf::from(home).join(".local/state/chatgpt-bridge/checkpoints"))
}

fn is_vcs_metadata(name: &OsStr) -> bool {
    VCS_METADATA_DIRS
        .iter()
        .any(|ignored| name == OsStr::new(ignored))
}

fn is_generated_dir(name: &OsStr) -> bool {
    GENERATED_DIRS
        .iter()
        .any(|ignored| name == OsStr::new(ignored))
}

fn env_bool(name: &str, default: bool) -> Result<bool, ApiError> {
    let Some(raw) = env::var_os(name) else {
        return Ok(default);
    };
    match raw.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ApiError::bad_request(format!(
            "invalid {name}; expected true or false"
        ))),
    }
}

fn relative_utf8(root: &Path, path: &Path) -> Result<String, ApiError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ApiError::forbidden("checkpoint path escapes its scope"))?;
    relative.to_str().map(str::to_owned).ok_or_else(|| {
        ApiError::bad_request(format!("workspace path is not valid UTF-8: {relative:?}"))
    })
}

fn diff_summary(
    before: &BTreeMap<String, Entry>,
    after: &BTreeMap<String, Entry>,
) -> ChangeSummary {
    let mut summary = ChangeSummary::default();
    for (path, entry) in after {
        match before.get(path) {
            None => summary.created += 1,
            Some(previous) if previous != entry => summary.modified += 1,
            Some(_) => {}
        }
    }
    for path in before.keys() {
        if !after.contains_key(path) {
            summary.deleted += 1;
        }
    }
    summary
}

fn same_kind(left: &Entry, right: &Entry) -> bool {
    matches!(
        (left, right),
        (Entry::File { .. }, Entry::File { .. })
            | (Entry::Directory { .. }, Entry::Directory { .. })
            | (Entry::Symlink { .. }, Entry::Symlink { .. })
    )
}

fn path_depth(path: &str) -> usize {
    Path::new(path).components().count()
}

fn remove_non_directory(path: &Path, relative: &str) -> Result<(), ApiError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => fs::remove_file(path).map_err(io_internal(
            "failed to replace path with directory during restore",
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ApiError::internal(format!(
            "failed to inspect {relative:?}: {error}"
        ))),
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), ApiError> {
    let parent = path
        .parent()
        .ok_or_else(|| ApiError::internal("checkpoint path has no parent"))?;
    fs::create_dir_all(parent)
        .map_err(io_internal("failed to create checkpoint parent directory"))?;
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or("checkpoint");
    let temp = parent.join(format!(".{name}.tmp-{}-{}", std::process::id(), now_ms()));

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true).mode(0o600);
    let mut file = options
        .open(&temp)
        .map_err(io_internal("failed to create temporary checkpoint file"))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temp);
        return Err(ApiError::internal(format!(
            "failed to write temporary checkpoint file: {error}"
        )));
    }
    drop(file);

    if let Err(error) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(ApiError::internal(format!(
            "failed to install checkpoint file atomically: {error}"
        )));
    }
    Ok(())
}

fn create_private_dir_all(path: &Path) -> Result<(), ApiError> {
    fs::create_dir_all(path).map_err(io_internal("failed to create checkpoint directory"))?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(io_internal("failed to secure checkpoint directory"))?;
    Ok(())
}

fn digest_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn io_internal(context: &'static str) -> impl FnOnce(io::Error) -> ApiError {
    move |error| ApiError::internal(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::CheckpointStore;
    use std::fs;
    use tempfile::TempDir;

    fn store() -> (TempDir, TempDir, CheckpointStore) {
        let workspace = TempDir::new().unwrap();
        let state = TempDir::new().unwrap();
        let store = CheckpointStore::with_state_root(
            workspace.path(),
            state.path().join("checkpoints"),
            false,
        )
        .unwrap();
        (workspace, state, store)
    }

    #[test]
    fn checkpoint_and_restore_work_without_git() {
        let (workspace, _state, store) = store();
        fs::write(workspace.path().join("a.txt"), "one").unwrap();

        let tx1 = store.begin(workspace.path()).unwrap();
        fs::write(workspace.path().join("a.txt"), "two").unwrap();
        let first = store.finish(&tx1.transaction_id).unwrap();
        let first_id = first.change_id.unwrap();

        let tx2 = store.begin(workspace.path()).unwrap();
        fs::write(workspace.path().join("a.txt"), "three").unwrap();
        fs::write(workspace.path().join("b.txt"), "new").unwrap();
        store.finish(&tx2.transaction_id).unwrap();

        store.restore(&first_id, false).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "two"
        );
        assert!(!workspace.path().join("b.txt").exists());
    }

    #[test]
    fn vcs_metadata_is_never_restored() {
        let (workspace, _state, store) = store();
        fs::create_dir(workspace.path().join(".git")).unwrap();
        fs::write(workspace.path().join(".git/HEAD"), "before").unwrap();
        fs::write(workspace.path().join("a.txt"), "one").unwrap();

        let tx = store.begin(workspace.path()).unwrap();
        fs::write(workspace.path().join("a.txt"), "two").unwrap();
        let checkpoint = store.finish(&tx.transaction_id).unwrap().change_id.unwrap();

        fs::write(workspace.path().join(".git/HEAD"), "after").unwrap();
        store.restore(&checkpoint, false).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join(".git/HEAD")).unwrap(),
            "after"
        );
    }

    #[test]
    fn undo_restores_state_before_selected_change() {
        let (workspace, _state, store) = store();
        fs::write(workspace.path().join("a.txt"), "before").unwrap();
        let tx = store.begin(workspace.path()).unwrap();
        fs::write(workspace.path().join("a.txt"), "after").unwrap();
        let change_id = store.finish(&tx.transaction_id).unwrap().change_id.unwrap();

        store.undo(&change_id, false).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "before"
        );
    }

    #[test]
    fn force_restore_preserves_uncheckpointed_changes_in_safety_checkpoint() {
        let (workspace, _state, store) = store();
        fs::write(workspace.path().join("a.txt"), "one").unwrap();
        let tx = store.begin(workspace.path()).unwrap();
        fs::write(workspace.path().join("a.txt"), "two").unwrap();
        let checkpoint = store.finish(&tx.transaction_id).unwrap().change_id.unwrap();

        fs::write(workspace.path().join("a.txt"), "manual").unwrap();
        assert!(store.restore(&checkpoint, false).is_err());

        let restored = store.restore(&checkpoint, true).unwrap();
        assert!(restored.safety_checkpoint_id.is_some());
        assert_eq!(
            fs::read_to_string(workspace.path().join("a.txt")).unwrap(),
            "two"
        );
    }
    #[test]
    fn generated_directories_are_ignored_by_default() {
        let (workspace, _state, store) = store();
        fs::create_dir(workspace.path().join("target")).unwrap();
        fs::write(workspace.path().join("target/artifact"), "one").unwrap();

        let tx = store.begin(workspace.path()).unwrap();
        fs::write(workspace.path().join("target/artifact"), "two").unwrap();
        let result = store.finish(&tx.transaction_id).unwrap();

        assert!(!result.changed);
        assert_eq!(
            fs::read_to_string(workspace.path().join("target/artifact")).unwrap(),
            "two"
        );
    }

    #[test]
    fn finish_without_changes_does_not_create_visible_checkpoint() {
        let (workspace, _state, store) = store();
        fs::write(workspace.path().join("a.txt"), "one").unwrap();
        let tx = store.begin(workspace.path()).unwrap();
        let result = store.finish(&tx.transaction_id).unwrap();
        assert!(!result.changed);
        assert!(result.change_id.is_none());
        assert!(store.list().unwrap().is_empty());
    }
}
