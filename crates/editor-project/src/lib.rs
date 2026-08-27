//! Filesystem persistence for the canonical `editor_domain::ProjectDocument`.
//!
//! This crate owns project-file policy and recovery. Timeline rules stay in
//! `editor-domain`; this crate only decodes, validates, and stores that IR.

use editor_domain::{
    AssetStatus, DomainError, ProjectDocument, RelativePath, CURRENT_SCHEMA_VERSION,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    error::Error,
    ffi::OsString,
    fmt,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

pub const PROJECT_EXTENSION: &str = "vdeproj";
pub const TEMPORARY_SUFFIX: &str = ".tmp";
pub const BACKUP_SUFFIX: &str = ".bak";

#[derive(Debug)]
pub enum ProjectError {
    InvalidProjectPath {
        path: PathBuf,
        reason: String,
    },
    InvalidAssetReference {
        reference: String,
        reason: String,
    },
    PathOutsideProject {
        path: PathBuf,
    },
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    Domain {
        path: PathBuf,
        source: DomainError,
    },
    MissingSchemaVersion {
        path: PathBuf,
    },
    InvalidSchemaVersion {
        path: PathBuf,
    },
    UnsupportedSchema {
        path: PathBuf,
        found: u64,
        current: u32,
    },
    ReopenValidation {
        path: PathBuf,
        source: Box<ProjectError>,
    },
    RecoveryFailed {
        project_path: PathBuf,
        candidates: Vec<RecoveryFailure>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryFailure {
    pub path: PathBuf,
    pub reason: String,
}

impl fmt::Display for ProjectError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidProjectPath { path, reason } => {
                write!(
                    formatter,
                    "invalid project path {}: {reason}",
                    path.display()
                )
            }
            Self::InvalidAssetReference { reference, reason } => {
                write!(formatter, "invalid asset reference {reference:?}: {reason}")
            }
            Self::PathOutsideProject { path } => {
                write!(
                    formatter,
                    "asset path is outside project root: {}",
                    path.display()
                )
            }
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::Json { path, source } => {
                write!(
                    formatter,
                    "invalid project JSON {}: {source}",
                    path.display()
                )
            }
            Self::Domain { path, source } => {
                write!(
                    formatter,
                    "project validation failed {}: {source}",
                    path.display()
                )
            }
            Self::MissingSchemaVersion { path } => {
                write!(
                    formatter,
                    "project JSON has no schema_version: {}",
                    path.display()
                )
            }
            Self::InvalidSchemaVersion { path } => {
                write!(
                    formatter,
                    "project JSON has an invalid schema_version: {}",
                    path.display()
                )
            }
            Self::UnsupportedSchema {
                path,
                found,
                current,
            } => write!(
                formatter,
                "unsupported project schema {found} in {}; current schema is {current}",
                path.display()
            ),
            Self::ReopenValidation { path, source } => {
                write!(
                    formatter,
                    "temporary project failed reopen validation {}: {source}",
                    path.display()
                )
            }
            Self::RecoveryFailed {
                project_path,
                candidates,
            } => write!(
                formatter,
                "no recoverable project candidate for {} ({})",
                project_path.display(),
                candidates.len()
            ),
        }
    }
}

impl Error for ProjectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Json { source, .. } => Some(source),
            Self::Domain { source, .. } => Some(source),
            Self::ReopenValidation { source, .. } => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SaveResult {
    pub project_path: PathBuf,
    pub temporary_path: PathBuf,
    pub backup_path: Option<PathBuf>,
    pub bytes_written: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoverySource {
    Primary,
    Backup,
    Temporary,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RecoveredProject {
    pub document: ProjectDocument,
    pub source: RecoverySource,
    pub source_path: PathBuf,
}

/// Explicit boundary for schema evolution.
pub struct MigrationBoundary;

impl MigrationBoundary {
    /// Accepts only the current schema until a legacy schema is specified.
    ///
    /// Older documents are not guessed or silently reshaped. A future
    /// migration can add a version-specific branch here and still feed the
    /// same domain decoder and validator below.
    pub fn apply(value: Value, path: &Path) -> Result<Value, ProjectError> {
        let schema = value
            .get("schema_version")
            .ok_or_else(|| ProjectError::MissingSchemaVersion {
                path: path.to_path_buf(),
            })?
            .as_u64()
            .ok_or_else(|| ProjectError::InvalidSchemaVersion {
                path: path.to_path_buf(),
            })?;
        if schema != u64::from(CURRENT_SCHEMA_VERSION) {
            return Err(ProjectError::UnsupportedSchema {
                path: path.to_path_buf(),
                found: schema,
                current: CURRENT_SCHEMA_VERSION,
            });
        }
        Ok(value)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProjectPersistence;

impl ProjectPersistence {
    pub fn new() -> Self {
        Self
    }

    pub fn load(&self, project_path: impl AsRef<Path>) -> Result<ProjectDocument, ProjectError> {
        load_project(project_path)
    }

    pub fn save(
        &self,
        project_path: impl AsRef<Path>,
        document: &ProjectDocument,
    ) -> Result<SaveResult, ProjectError> {
        save_project(project_path, document)
    }

    pub fn recover(
        &self,
        project_path: impl AsRef<Path>,
    ) -> Result<RecoveredProject, ProjectError> {
        recover_project(project_path)
    }
}

pub fn project_temporary_path(project_path: impl AsRef<Path>) -> PathBuf {
    sibling_with_suffix(project_path.as_ref(), TEMPORARY_SUFFIX)
}

pub fn project_backup_path(project_path: impl AsRef<Path>) -> PathBuf {
    sibling_with_suffix(project_path.as_ref(), BACKUP_SUFFIX)
}

pub fn validate_project_path(project_path: impl AsRef<Path>) -> Result<PathBuf, ProjectError> {
    let path = project_path.as_ref();
    if path.file_name().is_none() {
        return Err(ProjectError::InvalidProjectPath {
            path: path.to_path_buf(),
            reason: "must name a project file".to_owned(),
        });
    }
    let extension_is_valid = path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(PROJECT_EXTENSION));
    if !extension_is_valid {
        return Err(ProjectError::InvalidProjectPath {
            path: path.to_path_buf(),
            reason: format!("must use .{PROJECT_EXTENSION} extension"),
        });
    }
    Ok(path.to_path_buf())
}

pub fn validate_asset_reference(value: impl Into<String>) -> Result<RelativePath, ProjectError> {
    let reference = value.into();
    RelativePath::new(reference.clone()).map_err(|error| ProjectError::InvalidAssetReference {
        reference,
        reason: error.to_string(),
    })
}

pub fn resolve_asset_path(
    project_path: impl AsRef<Path>,
    relative_path: &RelativePath,
) -> Result<PathBuf, ProjectError> {
    let project_path = validate_project_path(project_path)?;
    resolve_asset_path_unchecked(&project_path, relative_path)
}

fn resolve_asset_path_unchecked(
    project_path: &Path,
    relative_path: &RelativePath,
) -> Result<PathBuf, ProjectError> {
    relative_path
        .validate()
        .map_err(|error| ProjectError::InvalidAssetReference {
            reference: relative_path.as_str().to_owned(),
            reason: error.to_string(),
        })?;

    let root = project_path.parent().unwrap_or_else(|| Path::new("."));
    let canonical_root = fs::canonicalize(root).map_err(|source| ProjectError::Io {
        operation: "canonicalize project root",
        path: root.to_path_buf(),
        source,
    })?;
    let candidate = append_relative_reference(&canonical_root, relative_path.as_str());
    let (existing, missing) = nearest_existing_ancestor(&candidate);
    let canonical_existing = fs::canonicalize(&existing).map_err(|source| ProjectError::Io {
        operation: "canonicalize asset path",
        path: existing.clone(),
        source,
    })?;
    if !canonical_existing.starts_with(&canonical_root) {
        return Err(ProjectError::PathOutsideProject { path: candidate });
    }

    let mut resolved = canonical_existing;
    for component in missing.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

pub fn load_project(project_path: impl AsRef<Path>) -> Result<ProjectDocument, ProjectError> {
    let project_path = validate_project_path(project_path)?;
    let mut document = read_document(&project_path)?;
    refresh_asset_statuses(&mut document, &project_path)?;
    Ok(document)
}

pub fn save_project(
    project_path: impl AsRef<Path>,
    document: &ProjectDocument,
) -> Result<SaveResult, ProjectError> {
    let project_path = validate_project_path(project_path)?;
    let lock = save_lock_for(&project_path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    save_project_locked(&project_path, document)
}

/// Saves only when the on-disk project still has the expected revision.
///
/// The revision check and complete temporary/reopen/backup/replace sequence
/// share one per-project lock.
pub fn save_project_if_revision(
    project_path: impl AsRef<Path>,
    document: &ProjectDocument,
    expected_revision: u64,
) -> Result<SaveResult, ProjectError> {
    let project_path = validate_project_path(project_path)?;
    let lock = save_lock_for(&project_path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    validate_save_destination(&project_path)?;

    if project_path.is_file() {
        let current = read_document(&project_path)?;
        if current.revision != expected_revision {
            return Err(revision_conflict(
                &project_path,
                expected_revision,
                current.revision,
            ));
        }
    }

    save_project_locked(&project_path, document)
}

fn save_project_locked(
    project_path: &Path,
    document: &ProjectDocument,
) -> Result<SaveResult, ProjectError> {
    validate_save_destination(project_path)?;
    document.validate().map_err(|source| ProjectError::Domain {
        path: project_path.to_path_buf(),
        source,
    })?;
    validate_document_asset_paths(document, project_path)?;

    let json = canonical_json(document, project_path)?;
    let temporary_path = project_temporary_path(project_path);
    write_temporary(&temporary_path, json.as_bytes())?;

    if let Err(source) = read_document(&temporary_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(ProjectError::ReopenValidation {
            path: temporary_path,
            source: Box::new(source),
        });
    }

    let had_previous_project = project_path.is_file();
    let backup_path = if had_previous_project {
        let backup_path = project_backup_path(project_path);
        copy_and_sync(project_path, &backup_path)?;
        Some(backup_path)
    } else {
        None
    };

    if let Err(source) = atomic_replace(&temporary_path, project_path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(ProjectError::Io {
            operation: "atomically replace project",
            path: project_path.to_path_buf(),
            source,
        });
    }

    Ok(SaveResult {
        project_path: project_path.to_path_buf(),
        temporary_path,
        backup_path,
        bytes_written: json.len(),
    })
}

fn revision_conflict(path: &Path, expected: u64, actual: u64) -> ProjectError {
    ProjectError::Domain {
        path: path.to_path_buf(),
        source: DomainError::RevisionConflict { expected, actual },
    }
}

fn save_lock_for(project_path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = save_lock_key(project_path);
    let mut locks = locks
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(key).or_insert_with(|| Arc::new(Mutex::new(()))))
}

fn save_lock_key(project_path: &Path) -> PathBuf {
    let parent = project_path.parent().unwrap_or_else(|| Path::new("."));
    let parent = fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf());
    parent.join(
        project_path
            .file_name()
            .unwrap_or_else(|| std::ffi::OsStr::new("project")),
    )
}

fn validate_save_destination(project_path: &Path) -> Result<(), ProjectError> {
    let parent = project_path.parent().unwrap_or_else(|| Path::new("."));
    let parent_metadata = fs::metadata(parent).map_err(|source| ProjectError::Io {
        operation: "inspect project destination",
        path: parent.to_path_buf(),
        source,
    })?;
    if !parent_metadata.is_dir() {
        return Err(ProjectError::InvalidProjectPath {
            path: project_path.to_path_buf(),
            reason: "project destination parent must be a directory".to_owned(),
        });
    }

    validate_existing_destination(project_path, "project destination")?;
    validate_existing_destination(
        &project_temporary_path(project_path),
        "project temporary file",
    )?;
    validate_existing_destination(&project_backup_path(project_path), "project backup file")?;
    Ok(())
}

fn validate_existing_destination(path: &Path, label: &str) -> Result<(), ProjectError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(ProjectError::InvalidProjectPath {
                path: path.to_path_buf(),
                reason: format!("{label} must not be a symlink"),
            })
        }
        Ok(metadata) if !metadata.is_file() => Err(ProjectError::InvalidProjectPath {
            path: path.to_path_buf(),
            reason: format!("{label} must be a regular file"),
        }),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ProjectError::Io {
            operation: "inspect project destination",
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn recover_project(project_path: impl AsRef<Path>) -> Result<RecoveredProject, ProjectError> {
    let project_path = validate_project_path(project_path)?;
    let candidates = [
        (RecoverySource::Primary, project_path.clone()),
        (RecoverySource::Backup, project_backup_path(&project_path)),
        (
            RecoverySource::Temporary,
            project_temporary_path(&project_path),
        ),
    ];
    let mut failures = Vec::new();
    for (source, path) in candidates {
        match read_document(&path).and_then(|mut document| {
            refresh_asset_statuses(&mut document, &project_path)?;
            Ok(document)
        }) {
            Ok(document) => {
                return Ok(RecoveredProject {
                    document,
                    source,
                    source_path: path,
                });
            }
            Err(error) => failures.push(RecoveryFailure {
                path,
                reason: error.to_string(),
            }),
        }
    }
    Err(ProjectError::RecoveryFailed {
        project_path,
        candidates: failures,
    })
}

fn read_document(path: &Path) -> Result<ProjectDocument, ProjectError> {
    let bytes = fs::read(path).map_err(|source| ProjectError::Io {
        operation: "read project",
        path: path.to_path_buf(),
        source,
    })?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|source| ProjectError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    let value = MigrationBoundary::apply(value, path)?;
    let document: ProjectDocument =
        serde_json::from_value(value).map_err(|source| ProjectError::Json {
            path: path.to_path_buf(),
            source,
        })?;
    document.validate().map_err(|source| ProjectError::Domain {
        path: path.to_path_buf(),
        source,
    })?;
    validate_document_asset_paths(&document, path)?;
    Ok(document)
}

fn canonical_json(document: &ProjectDocument, project_path: &Path) -> Result<String, ProjectError> {
    let mut json = serde_json::to_string_pretty(document).map_err(|source| ProjectError::Json {
        path: project_path.to_path_buf(),
        source,
    })?;
    json.push('\n');
    Ok(json)
}

fn validate_document_asset_paths(
    document: &ProjectDocument,
    project_path: &Path,
) -> Result<(), ProjectError> {
    for asset in &document.assets {
        resolve_asset_path_unchecked(project_path, &asset.relative_path)?;
    }
    Ok(())
}

fn refresh_asset_statuses(
    document: &mut ProjectDocument,
    project_path: &Path,
) -> Result<(), ProjectError> {
    for asset in &mut document.assets {
        let resolved = resolve_asset_path_unchecked(project_path, &asset.relative_path)?;
        let status = match fs::metadata(&resolved) {
            Ok(metadata) if metadata.is_file() => {
                if asset.status == AssetStatus::Missing {
                    AssetStatus::Available
                } else {
                    asset.status.clone()
                }
            }
            Ok(_) | Err(_) => AssetStatus::Missing,
        };
        asset.status = status;
    }
    Ok(())
}

fn append_relative_reference(root: &Path, reference: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for component in reference.split(['/', '\\']) {
        if !component.is_empty() && component != "." {
            path.push(component);
        }
    }
    path
}

fn nearest_existing_ancestor(path: &Path) -> (PathBuf, Vec<OsString>) {
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    while !cursor.exists() {
        if let Some(name) = cursor.file_name() {
            missing.push(name.to_os_string());
            cursor.pop();
        } else {
            break;
        }
    }
    (cursor, missing)
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

fn write_temporary(path: &Path, bytes: &[u8]) -> Result<(), ProjectError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|source| ProjectError::Io {
            operation: "create temporary project",
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(bytes).map_err(|source| ProjectError::Io {
        operation: "write temporary project",
        path: path.to_path_buf(),
        source,
    })?;
    file.sync_all().map_err(|source| ProjectError::Io {
        operation: "flush temporary project",
        path: path.to_path_buf(),
        source,
    })
}

fn copy_and_sync(source: &Path, destination: &Path) -> Result<(), ProjectError> {
    fs::copy(source, destination).map_err(|source| ProjectError::Io {
        operation: "write project backup",
        path: destination.to_path_buf(),
        source,
    })?;
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(destination)
        .and_then(|file| file.sync_all())
        .map_err(|source| ProjectError::Io {
            operation: "flush project backup",
            path: destination.to_path_buf(),
            source,
        })
}

fn atomic_replace(temporary: &Path, destination: &Path) -> Result<(), io::Error> {
    #[cfg(windows)]
    {
        if destination.exists() {
            return replace_existing_windows(temporary, destination);
        }
    }
    fs::rename(temporary, destination)
}

#[cfg(windows)]
fn replace_existing_windows(temporary: &Path, destination: &Path) -> Result<(), io::Error> {
    use std::{os::windows::ffi::OsStrExt, ptr};

    #[link(name = "kernel32")]
    extern "system" {
        fn ReplaceFileW(
            replaced_file_name: *const u16,
            replacement_file_name: *const u16,
            backup_file_name: *const u16,
            replace_flags: u32,
            exclude: *mut std::ffi::c_void,
            reserved: *mut std::ffi::c_void,
        ) -> i32;
    }

    let destination_wide: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let temporary_wide: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replaced = unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            temporary_wide.as_ptr(),
            ptr::null(),
            0,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_domain::{
        Asset, AssetId, AssetKind, Clip, Fingerprint, ProjectId, Rational, Track, TrackId,
        TrackKind, Transform,
    };
    use std::{
        sync::{Arc, Barrier},
        time::{SystemTime, UNIX_EPOCH},
    };

    fn test_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be valid")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("video-editor-project-{nonce}"));
        fs::create_dir_all(&path).expect("test directory must be created");
        path
    }

    fn project(asset_status: AssetStatus) -> ProjectDocument {
        let project_id = ProjectId::new("project-1").expect("valid project ID");
        let asset_id = AssetId::new("asset-1").expect("valid asset ID");
        let track_id = TrackId::new("video-1").expect("valid track ID");
        let mut document = ProjectDocument::create(project_id, "Test project").unwrap();
        document.assets.push(Asset {
            id: asset_id.clone(),
            relative_path: RelativePath::new("media/video.mp4").unwrap(),
            kind: AssetKind::Video,
            fingerprint: Fingerprint {
                size_bytes: 10,
                modified_time: "2026-08-27T00:00:00Z".to_owned(),
                sha256: None,
            },
            probe: None,
            status: asset_status,
        });
        document
            .sequence
            .tracks
            .push(Track::new(track_id, TrackKind::Video, "Video").unwrap());
        document.sequence.tracks[0].clips.push(Clip {
            id: editor_domain::ClipId::new("clip-1").expect("valid clip ID"),
            asset_id,
            timeline_start: 0,
            timeline_duration: 30,
            source_start: 0,
            source_duration: 30,
            speed: Rational::new(1, 1).unwrap(),
            opacity: 1.0,
            transform: Transform::default(),
            effects: Vec::new(),
            keyframes: Vec::new(),
        });
        document.validate().unwrap();
        document
    }

    fn project_path(directory: &Path) -> PathBuf {
        directory.join("project.vdeproj")
    }

    #[test]
    fn roundtrip_writes_canonical_json_and_reopens_domain_document() {
        let directory = test_directory();
        fs::create_dir(directory.join("media")).unwrap();
        fs::write(directory.join("media/video.mp4"), b"real fixture marker").unwrap();
        let path = project_path(&directory);
        let document = project(AssetStatus::Available);

        let result = save_project(&path, &document).unwrap();
        let loaded = load_project(&path).unwrap();

        assert_eq!(loaded, document);
        assert!(result.backup_path.is_none());
        assert!(!result.temporary_path.exists());
        assert!(fs::read_to_string(path).unwrap().ends_with('\n'));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn second_save_keeps_backup_and_recovery_uses_it_after_primary_corruption() {
        let directory = test_directory();
        fs::create_dir(directory.join("media")).unwrap();
        fs::write(directory.join("media/video.mp4"), b"real fixture marker").unwrap();
        let path = project_path(&directory);
        let first = project(AssetStatus::Available);
        save_project(&path, &first).unwrap();
        let mut second = first.clone();
        second.name = "Second project".to_owned();
        save_project(&path, &second).unwrap();

        assert_eq!(
            fs::read_to_string(project_backup_path(&path)).unwrap(),
            canonical_json(&first, &path).unwrap()
        );
        fs::write(&path, b"{not-json").unwrap();
        let recovered = recover_project(&path).unwrap();
        assert_eq!(recovered.source, RecoverySource::Backup);
        assert_eq!(recovered.document, first);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn stale_revision_is_rejected_without_replacing_newer_project() {
        let directory = test_directory();
        fs::create_dir(directory.join("media")).unwrap();
        fs::write(directory.join("media/video.mp4"), b"real fixture marker").unwrap();
        let path = project_path(&directory);
        let first = project(AssetStatus::Available);
        save_project(&path, &first).unwrap();

        let mut newer = first.clone();
        newer.name = "Newer project".to_owned();
        newer.revision = 1;
        save_project_if_revision(&path, &newer, 0).unwrap();

        let error = save_project_if_revision(&path, &first, 0).unwrap_err();
        assert!(matches!(
            error,
            ProjectError::Domain {
                source: DomainError::RevisionConflict {
                    expected: 0,
                    actual: 1
                },
                ..
            }
        ));
        assert_eq!(load_project(&path).unwrap(), newer);
        assert!(!project_temporary_path(&path).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn concurrent_saves_share_one_deterministic_temp_sequence() {
        let directory = test_directory();
        fs::create_dir(directory.join("media")).unwrap();
        fs::write(directory.join("media/video.mp4"), b"real fixture marker").unwrap();
        let path = Arc::new(project_path(&directory));
        let document = Arc::new(project(AssetStatus::Available));
        save_project(path.as_ref(), document.as_ref()).unwrap();
        let barrier = Arc::new(Barrier::new(2));

        std::thread::scope(|scope| {
            let first_path = Arc::clone(&path);
            let first_document = Arc::clone(&document);
            let first_barrier = Arc::clone(&barrier);
            let first = scope.spawn(move || {
                first_barrier.wait();
                save_project(first_path.as_ref(), first_document.as_ref())
            });

            let second_path = Arc::clone(&path);
            let second_document = Arc::clone(&document);
            let second_barrier = Arc::clone(&barrier);
            let second = scope.spawn(move || {
                second_barrier.wait();
                save_project(second_path.as_ref(), second_document.as_ref())
            });

            first.join().unwrap().unwrap();
            second.join().unwrap().unwrap();
        });

        assert_eq!(load_project(path.as_ref()).unwrap(), *document);
        assert!(!project_temporary_path(path.as_ref()).exists());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_json_returns_typed_error() {
        let directory = test_directory();
        let path = project_path(&directory);
        fs::write(&path, b"{not-json").unwrap();

        assert!(matches!(
            load_project(&path),
            Err(ProjectError::Json { .. })
        ));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn missing_asset_is_preserved_and_marked_missing() {
        let directory = test_directory();
        fs::create_dir(directory.join("media")).unwrap();
        let path = project_path(&directory);
        let document = project(AssetStatus::Available);
        save_project(&path, &document).unwrap();

        let loaded = load_project(&path).unwrap();
        assert_eq!(loaded.assets.len(), 1);
        assert_eq!(loaded.assets[0].status, AssetStatus::Missing);
        assert_eq!(loaded.sequence.tracks[0].clips.len(), 1);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn traversal_and_absolute_asset_references_are_rejected_by_domain_policy() {
        assert!(matches!(
            validate_asset_reference("../outside.mp4"),
            Err(ProjectError::InvalidAssetReference { .. })
        ));
        assert!(matches!(
            validate_asset_reference("C:\\outside.mp4"),
            Err(ProjectError::InvalidAssetReference { .. })
        ));
        assert!(matches!(
            validate_project_path("project.json"),
            Err(ProjectError::InvalidProjectPath { .. })
        ));
    }

    #[test]
    fn migration_boundary_rejects_unregistered_schema_versions() {
        let directory = test_directory();
        let path = project_path(&directory);
        let mut value = serde_json::to_value(project(AssetStatus::Missing)).unwrap();
        value["schema_version"] = Value::from(0);
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();

        assert!(matches!(
            load_project(&path),
            Err(ProjectError::UnsupportedSchema { found: 0, .. })
        ));
        fs::remove_dir_all(directory).unwrap();
    }
}
