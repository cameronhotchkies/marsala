use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Error};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::{watch, Mutex};

const SETTINGS_VERSION: u32 = 1;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeSettingsSnapshot {
    pub revision: u64,
    pub goblin_mode: bool,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuntimeSettingsUpdate {
    pub goblin_mode: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeSettingsLoadIssue {
    Unreadable,
    TooLarge,
    Malformed,
    UnsupportedVersion,
}

impl RuntimeSettingsLoadIssue {
    pub fn code(self) -> &'static str {
        match self {
            Self::Unreadable => "unreadable",
            Self::TooLarge => "too_large",
            Self::Malformed => "malformed",
            Self::UnsupportedVersion => "unsupported_version",
        }
    }
}

#[derive(Debug)]
pub struct RuntimeSettingsLoad {
    pub handle: RuntimeSettingsHandle,
    pub issue: Option<RuntimeSettingsLoadIssue>,
}

#[derive(Clone)]
pub struct RuntimeSettingsHandle {
    inner: Arc<RuntimeSettingsInner>,
}

impl fmt::Debug for RuntimeSettingsHandle {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimeSettingsHandle")
            .field("path", &self.inner.path)
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

struct RuntimeSettingsInner {
    path: PathBuf,
    state: Mutex<RuntimeSettingsState>,
    changes: watch::Sender<RuntimeSettingsSnapshot>,
    #[cfg(test)]
    directory_sync_failures: AtomicU64,
}

#[derive(Debug, Clone, Copy)]
struct RuntimeSettingsState {
    snapshot: RuntimeSettingsSnapshot,
    needs_repair: bool,
}

#[derive(Debug)]
pub enum RuntimeSettingsUpdateError {
    Conflict { expected: u64, actual: u64 },
    RevisionExhausted,
    Persistence(Error),
}

impl RuntimeSettingsUpdateError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Conflict { .. } => "revision_conflict",
            Self::RevisionExhausted => "revision_exhausted",
            Self::Persistence(_) => "persistence_failed",
        }
    }
}

impl fmt::Display for RuntimeSettingsUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict { expected, actual } => write!(
                formatter,
                "runtime settings revision conflict: expected {expected}, actual {actual}"
            ),
            Self::RevisionExhausted => formatter.write_str("runtime settings revision exhausted"),
            Self::Persistence(_) => formatter.write_str("failed to persist runtime settings"),
        }
    }
}

impl std::error::Error for RuntimeSettingsUpdateError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Persistence(error) => Some(error.as_ref()),
            _ => None,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedRuntimeSettings {
    version: u32,
    revision: u64,
    goblin_mode: bool,
}

impl RuntimeSettingsHandle {
    pub async fn load(path: impl Into<PathBuf>) -> RuntimeSettingsLoad {
        let path = path.into();
        let (snapshot, issue) = load_snapshot(&path).await;
        let needs_repair = issue.is_some();
        let (changes, _) = watch::channel(snapshot);

        RuntimeSettingsLoad {
            handle: Self {
                inner: Arc::new(RuntimeSettingsInner {
                    path,
                    state: Mutex::new(RuntimeSettingsState {
                        snapshot,
                        needs_repair,
                    }),
                    changes,
                    #[cfg(test)]
                    directory_sync_failures: AtomicU64::new(0),
                }),
            },
            issue,
        }
    }

    pub fn snapshot(&self) -> RuntimeSettingsSnapshot {
        *self.inner.changes.borrow()
    }

    pub fn subscribe(&self) -> watch::Receiver<RuntimeSettingsSnapshot> {
        self.inner.changes.subscribe()
    }

    pub async fn update(
        &self,
        expected_revision: u64,
        update: RuntimeSettingsUpdate,
    ) -> Result<RuntimeSettingsSnapshot, RuntimeSettingsUpdateError> {
        let mut state = self.inner.state.lock().await;
        if expected_revision != state.snapshot.revision {
            return Err(RuntimeSettingsUpdateError::Conflict {
                expected: expected_revision,
                actual: state.snapshot.revision,
            });
        }

        if update.goblin_mode == state.snapshot.goblin_mode && !state.needs_repair {
            return Ok(state.snapshot);
        }

        let revision = state
            .snapshot
            .revision
            .checked_add(1)
            .ok_or(RuntimeSettingsUpdateError::RevisionExhausted)?;
        let next = RuntimeSettingsSnapshot {
            revision,
            goblin_mode: update.goblin_mode,
        };

        persist_snapshot(
            &self.inner.path,
            next,
            #[cfg(test)]
            &self.inner.directory_sync_failures,
        )
        .await
        .map_err(RuntimeSettingsUpdateError::Persistence)?;

        state.snapshot = next;
        state.needs_repair = false;
        self.inner.changes.send_replace(next);
        Ok(next)
    }
}

async fn load_snapshot(path: &Path) -> (RuntimeSettingsSnapshot, Option<RuntimeSettingsLoadIssue>) {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (RuntimeSettingsSnapshot::default(), None);
        }
        Err(_) => {
            return (
                RuntimeSettingsSnapshot::default(),
                Some(RuntimeSettingsLoadIssue::Unreadable),
            );
        }
    };

    if metadata.len() > MAX_SETTINGS_BYTES {
        return (
            RuntimeSettingsSnapshot::default(),
            Some(RuntimeSettingsLoadIssue::TooLarge),
        );
    }

    let bytes = match tokio::fs::read(path).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return (
                RuntimeSettingsSnapshot::default(),
                Some(RuntimeSettingsLoadIssue::Unreadable),
            );
        }
    };
    let persisted: PersistedRuntimeSettings = match serde_json::from_slice(&bytes) {
        Ok(persisted) => persisted,
        Err(_) => {
            return (
                RuntimeSettingsSnapshot::default(),
                Some(RuntimeSettingsLoadIssue::Malformed),
            );
        }
    };

    if persisted.version != SETTINGS_VERSION {
        return (
            RuntimeSettingsSnapshot::default(),
            Some(RuntimeSettingsLoadIssue::UnsupportedVersion),
        );
    }

    (
        RuntimeSettingsSnapshot {
            revision: persisted.revision,
            goblin_mode: persisted.goblin_mode,
        },
        None,
    )
}

async fn persist_snapshot(
    path: &Path,
    snapshot: RuntimeSettingsSnapshot,
    #[cfg(test)] directory_sync_failures: &AtomicU64,
) -> Result<(), Error> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| "failed to create runtime settings directory")?;
    }

    let persisted = PersistedRuntimeSettings {
        version: SETTINGS_VERSION,
        revision: snapshot.revision,
        goblin_mode: snapshot.goblin_mode,
    };
    let bytes =
        serde_json::to_vec_pretty(&persisted).context("failed to serialize runtime settings")?;
    let temp_path = temporary_path(path);
    let backup_path = backup_path(path);

    let write_result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .await
            .context("failed to create temporary runtime settings file")?;
        file.write_all(&bytes)
            .await
            .context("failed to write temporary runtime settings file")?;
        file.write_all(b"\n")
            .await
            .context("failed to finish temporary runtime settings file")?;
        file.flush()
            .await
            .context("failed to flush temporary runtime settings file")?;
        file.sync_all()
            .await
            .context("failed to sync temporary runtime settings file")?;
        drop(file);

        let had_previous = match tokio::fs::copy(path, &backup_path).await {
            Ok(_) => {
                let backup = tokio::fs::OpenOptions::new()
                    .read(true)
                    .open(&backup_path)
                    .await
                    .context("failed to open runtime settings backup")?;
                backup
                    .sync_all()
                    .await
                    .context("failed to sync runtime settings backup")?;
                sync_parent_directory(
                    parent,
                    #[cfg(test)]
                    directory_sync_failures,
                )
                .await
                .context("failed to sync runtime settings backup entry")?;
                true
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => {
                return Err(Error::new(error).context("failed to back up runtime settings file"));
            }
        };

        tokio::fs::rename(&temp_path, path)
            .await
            .context("failed to replace runtime settings file")?;

        if let Err(commit_error) = sync_parent_directory(
            parent,
            #[cfg(test)]
            directory_sync_failures,
        )
        .await
        {
            let rollback_result = if had_previous {
                tokio::fs::rename(&backup_path, path)
                    .await
                    .context("failed to restore previous runtime settings file")
            } else {
                tokio::fs::remove_file(path)
                    .await
                    .context("failed to remove uncommitted runtime settings file")
            };
            if rollback_result.is_err() {
                // The replacement remains the live atomic file. Treat it as
                // committed so callers publish the same state that is on disk.
                return Ok(());
            }
            sync_parent_directory(
                parent,
                #[cfg(test)]
                directory_sync_failures,
            )
            .await
            .context("failed to sync runtime settings rollback")?;
            return Err(commit_error.context("failed to commit runtime settings replacement"));
        }

        if had_previous {
            let _ = tokio::fs::remove_file(&backup_path).await;
        }
        Ok::<(), Error>(())
    }
    .await;

    if write_result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    write_result
}

fn temporary_path(path: &Path) -> PathBuf {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime-settings.json");
    path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        sequence
    ))
}

fn backup_path(path: &Path) -> PathBuf {
    let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("runtime-settings.json");
    path.with_file_name(format!(
        ".{file_name}.{}.{}.backup",
        std::process::id(),
        sequence
    ))
}

#[cfg(unix)]
async fn sync_parent_directory(
    parent: Option<&Path>,
    #[cfg(test)] directory_sync_failures: &AtomicU64,
) -> Result<(), Error> {
    #[cfg(test)]
    if matches!(
        directory_sync_failures.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
            remaining.checked_sub(1)
        }),
        Ok(1)
    ) {
        return Err(anyhow::anyhow!(
            "injected runtime settings directory sync failure"
        ));
    }
    let parent = parent.unwrap_or_else(|| Path::new("."));
    let parent = parent.to_owned();
    tokio::task::spawn_blocking(move || {
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .context("failed to sync runtime settings directory")
    })
    .await
    .context("runtime settings directory sync task failed")??;
    Ok(())
}

#[cfg(not(unix))]
async fn sync_parent_directory(
    _parent: Option<&Path>,
    #[cfg(test)] directory_sync_failures: &AtomicU64,
) -> Result<(), Error> {
    #[cfg(test)]
    if matches!(
        directory_sync_failures.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
            remaining.checked_sub(1)
        }),
        Ok(1)
    ) {
        return Err(anyhow::anyhow!(
            "injected runtime settings directory sync failure"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_file_uses_disabled_defaults() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("state/settings.json");

        let loaded = RuntimeSettingsHandle::load(&path).await;

        assert_eq!(loaded.issue, None);
        assert_eq!(loaded.handle.snapshot(), RuntimeSettingsSnapshot::default());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn update_is_durable_and_notifies_subscribers() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("state/settings.json");
        let loaded = RuntimeSettingsHandle::load(&path).await;
        let mut changes = loaded.handle.subscribe();

        let updated = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: true })
            .await
            .expect("update settings");

        assert_eq!(updated.revision, 1);
        assert!(updated.goblin_mode);
        changes.changed().await.expect("settings notification");
        assert_eq!(*changes.borrow(), updated);

        let reloaded = RuntimeSettingsHandle::load(&path).await;
        assert_eq!(reloaded.issue, None);
        assert_eq!(reloaded.handle.snapshot(), updated);
    }

    #[tokio::test]
    async fn stale_update_does_not_change_memory_or_disk() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        let loaded = RuntimeSettingsHandle::load(&path).await;
        let first = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: true })
            .await
            .expect("first update");

        let error = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: false })
            .await
            .expect_err("stale update must fail");

        assert!(matches!(
            error,
            RuntimeSettingsUpdateError::Conflict {
                expected: 0,
                actual: 1
            }
        ));
        assert_eq!(loaded.handle.snapshot(), first);
        assert_eq!(
            RuntimeSettingsHandle::load(&path).await.handle.snapshot(),
            first
        );
    }

    #[tokio::test]
    async fn cloned_handles_serialize_revisioned_updates() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        let loaded = RuntimeSettingsHandle::load(&path).await;
        let first = loaded.handle.clone();
        let second = loaded.handle.clone();

        let (first_result, second_result) = tokio::join!(
            first.update(0, RuntimeSettingsUpdate { goblin_mode: true }),
            second.update(0, RuntimeSettingsUpdate { goblin_mode: true })
        );

        let successes = [&first_result, &second_result]
            .into_iter()
            .filter(|result| result.is_ok())
            .count();
        let conflicts = [first_result, second_result]
            .into_iter()
            .filter(|result| {
                matches!(
                    result,
                    Err(RuntimeSettingsUpdateError::Conflict {
                        expected: 0,
                        actual: 1
                    })
                )
            })
            .count();

        assert_eq!(successes, 1);
        assert_eq!(conflicts, 1);
        assert_eq!(loaded.handle.snapshot().revision, 1);
        assert!(loaded.handle.snapshot().goblin_mode);
    }

    #[tokio::test]
    async fn corrupt_file_fails_safe_and_is_preserved_until_update() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        tokio::fs::write(&path, b"not json")
            .await
            .expect("write corrupt settings");

        let loaded = RuntimeSettingsHandle::load(&path).await;

        assert_eq!(loaded.issue, Some(RuntimeSettingsLoadIssue::Malformed));
        assert_eq!(loaded.handle.snapshot(), RuntimeSettingsSnapshot::default());
        assert_eq!(
            tokio::fs::read(&path).await.expect("read corrupt file"),
            b"not json"
        );

        let repaired = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: false })
            .await
            .expect("repair settings");
        assert_eq!(repaired.revision, 1);
        assert_eq!(RuntimeSettingsHandle::load(&path).await.issue, None);
    }

    #[tokio::test]
    async fn unsupported_version_fails_safe() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        tokio::fs::write(&path, br#"{"version":2,"revision":42,"goblin_mode":true}"#)
            .await
            .expect("write settings");

        let loaded = RuntimeSettingsHandle::load(&path).await;

        assert_eq!(
            loaded.issue,
            Some(RuntimeSettingsLoadIssue::UnsupportedVersion)
        );
        assert_eq!(loaded.handle.snapshot(), RuntimeSettingsSnapshot::default());
    }

    #[tokio::test]
    async fn failed_persistence_does_not_publish_update() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        tokio::fs::create_dir(&path)
            .await
            .expect("create destination directory");
        let loaded = RuntimeSettingsHandle::load(&path).await;
        let changes = loaded.handle.subscribe();

        let error = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: true })
            .await
            .expect_err("directory destination must fail");

        assert_eq!(error.code(), "persistence_failed");
        assert_eq!(loaded.handle.snapshot(), RuntimeSettingsSnapshot::default());
        assert!(changes.has_changed().is_ok_and(|changed| !changed));
    }

    #[tokio::test]
    async fn directory_sync_failure_restores_previous_disk_and_memory_state() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        let loaded = RuntimeSettingsHandle::load(&path).await;
        let first = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: true })
            .await
            .expect("initial update");
        let previous_bytes = tokio::fs::read(&path).await.expect("read initial settings");
        let changes = loaded.handle.subscribe();

        // The first directory sync preserves the backup; the second confirms
        // the replacement and is the failure boundary under test.
        loaded
            .handle
            .inner
            .directory_sync_failures
            .store(2, Ordering::Relaxed);
        let error = loaded
            .handle
            .update(first.revision, RuntimeSettingsUpdate { goblin_mode: false })
            .await
            .expect_err("commit sync failure must roll back");

        assert_eq!(error.code(), "persistence_failed");
        assert_eq!(loaded.handle.snapshot(), first);
        assert!(changes.has_changed().is_ok_and(|changed| !changed));
        assert_eq!(
            tokio::fs::read(&path)
                .await
                .expect("read rolled back settings"),
            previous_bytes
        );
        assert_eq!(
            RuntimeSettingsHandle::load(&path).await.handle.snapshot(),
            first
        );
    }

    #[tokio::test]
    async fn directory_sync_failure_removes_uncommitted_first_file() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        let loaded = RuntimeSettingsHandle::load(&path).await;

        loaded
            .handle
            .inner
            .directory_sync_failures
            .store(1, Ordering::Relaxed);
        let error = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: true })
            .await
            .expect_err("first commit sync failure must roll back");

        assert_eq!(error.code(), "persistence_failed");
        assert_eq!(loaded.handle.snapshot(), RuntimeSettingsSnapshot::default());
        assert!(!path.exists());
        assert_eq!(
            RuntimeSettingsHandle::load(&path).await.handle.snapshot(),
            RuntimeSettingsSnapshot::default()
        );
    }

    #[tokio::test]
    async fn failed_repair_preserves_corrupt_file_for_a_later_retry() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        let corrupt_bytes = b"not json";
        tokio::fs::write(&path, corrupt_bytes)
            .await
            .expect("write corrupt settings");
        let loaded = RuntimeSettingsHandle::load(&path).await;

        loaded
            .handle
            .inner
            .directory_sync_failures
            .store(2, Ordering::Relaxed);
        loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: false })
            .await
            .expect_err("failed repair must preserve corrupt input");

        assert_eq!(loaded.handle.snapshot(), RuntimeSettingsSnapshot::default());
        assert_eq!(
            tokio::fs::read(&path)
                .await
                .expect("read preserved corrupt file"),
            corrupt_bytes
        );

        let repaired = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: false })
            .await
            .expect("retry repair");
        assert_eq!(repaired.revision, 1);
        assert_eq!(RuntimeSettingsHandle::load(&path).await.issue, None);
    }

    #[tokio::test]
    async fn unchanged_clean_update_keeps_revision() {
        let tempdir = tempfile::tempdir().expect("tempdir");
        let path = tempdir.path().join("settings.json");
        let loaded = RuntimeSettingsHandle::load(&path).await;

        let snapshot = loaded
            .handle
            .update(0, RuntimeSettingsUpdate { goblin_mode: false })
            .await
            .expect("no-op update");

        assert_eq!(snapshot, RuntimeSettingsSnapshot::default());
        assert!(!path.exists());
    }
}
