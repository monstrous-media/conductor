// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! Local storage for captured patterns
//!
//! Note: This module is in early development - types defined but not fully used yet.

// Allow unused code during development phase
#![allow(dead_code)]

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::capture::{CaptureMetadata, CapturedEvent};
use crate::privacy::PrivacyLevel;

/// Stored capture pattern
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCapture {
    /// Version of the capture format
    pub version: String,

    /// Unique capture ID
    pub id: Uuid,

    /// Capture name
    pub name: String,

    /// Privacy level
    pub privacy: PrivacyLevel,

    /// When the capture was created
    pub created_at: DateTime<Utc>,

    /// Metadata
    pub metadata: CaptureMetadata,

    /// Captured events
    pub events: Vec<CapturedEvent>,
}

/// Storage manager for captures
pub struct CaptureStorage {
    base_dir: PathBuf,
}

impl CaptureStorage {
    /// Create a new storage manager
    pub fn new() -> Result<Self, std::io::Error> {
        let base_dir = Self::get_base_dir()?;

        // Ensure directory exists
        fs::create_dir_all(&base_dir)?;

        Ok(Self { base_dir })
    }

    /// Get the base directory for captures
    fn get_base_dir() -> Result<PathBuf, std::io::Error> {
        // Captures live under the same `conductor/` directory as the rest of
        // Conductor's data (see conductor-core's config loader, plugin registry
        // and signing modules) rather than a crate-private bundle id, which
        // would split capture files into a separate directory users never see
        // referenced anywhere else.
        let config_dir = dirs::config_dir().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "Could not determine config directory",
            )
        })?;

        Ok(config_dir.join("conductor").join("captures"))
    }

    /// Save a capture to local storage.
    ///
    /// This refuses to overwrite an existing capture file — if a capture
    /// with the same id is already on disk, it returns
    /// [`std::io::ErrorKind::AlreadyExists`] rather than silently truncating it.
    /// The previous `fs::write` clobbered the existing file with no warning, so
    /// an id reuse / double-save could destroy a stored capture unnoticed. Use
    /// [`save_overwrite`](Self::save_overwrite) to replace one deliberately.
    pub fn save(&self, capture: &StoredCapture) -> Result<PathBuf, std::io::Error> {
        self.write_capture(capture, false)
    }

    /// Save a capture, replacing any existing capture file with the same id.
    ///
    /// The explicit, opt-in counterpart to [`save`](Self::save) for
    /// callers that genuinely intend to update a capture in place. Overwriting
    /// is never the silent default.
    pub fn save_overwrite(&self, capture: &StoredCapture) -> Result<PathBuf, std::io::Error> {
        self.write_capture(capture, true)
    }

    /// Serialize `capture` to its `{id}.json` path via an atomic temp-write +
    /// rename. When `overwrite` is false the rename is non-clobbering
    /// (`AlreadyExists` if the file exists); when true it replaces any existing
    /// file. Shared by [`save`](Self::save) and
    /// [`save_overwrite`](Self::save_overwrite).
    ///
    /// The JSON is written to a temp file in the same directory and only then
    /// renamed into place, so the destination `{id}.json` only ever receives a
    /// complete file — a failed or partial write leaves no corrupt capture
    /// (the temp file is auto-removed on drop), and never a half-written file
    /// that would block future saves.
    fn write_capture(
        &self,
        capture: &StoredCapture,
        overwrite: bool,
    ) -> Result<PathBuf, std::io::Error> {
        let file_path = self.get_capture_path(&capture.id);

        let json = serde_json::to_string_pretty(capture)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        atomic_write_json(&file_path, &json, overwrite).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "capture {} already exists at {}; use save_overwrite to replace it",
                    capture.id,
                    file_path.display()
                ),
            ),
            _ => e,
        })?;

        Ok(file_path)
    }

    /// Load a capture by ID
    pub fn load(&self, id: &Uuid) -> Result<StoredCapture, std::io::Error> {
        let file_path = self.get_capture_path(id);
        let json = fs::read_to_string(&file_path)?;

        let capture: StoredCapture = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        Ok(capture)
    }

    /// Load a capture by name
    pub fn load_by_name(&self, name: &str) -> Result<StoredCapture, std::io::Error> {
        // Find capture with matching name
        for capture in self.list()? {
            if capture.name == name {
                return self.load(&capture.id);
            }
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Capture '{}' not found", name),
        ))
    }

    /// Delete a capture
    pub fn delete(&self, id: &Uuid) -> Result<(), std::io::Error> {
        let file_path = self.get_capture_path(id);
        fs::remove_file(file_path)?;
        Ok(())
    }

    /// List all captures.
    ///
    /// Corrupt/unreadable files are skipped but logged; use
    /// [`list_with_warnings`](Self::list_with_warnings) to surface them to a
    /// caller/UI. Previously read and JSON-parse errors were silently
    /// swallowed, so a corrupted capture simply vanished from listings —
    /// hiding data loss.
    pub fn list(&self) -> Result<Vec<CaptureInfo>, std::io::Error> {
        Ok(self.list_with_warnings()?.0)
    }

    /// List all captures, plus a warning for every `.json` file that could not
    /// be read or deserialized.
    ///
    /// Each warning names the offending path and the underlying error so the
    /// CLI/UI can tell the user which captures need recovery instead of letting
    /// them silently disappear. Warnings are also emitted via `tracing::warn!`.
    pub fn list_with_warnings(&self) -> Result<(Vec<CaptureInfo>, Vec<String>), std::io::Error> {
        let mut captures = Vec::new();
        let mut warnings = Vec::new();

        if !self.base_dir.exists() {
            return Ok((captures, warnings));
        }

        for entry in fs::read_dir(&self.base_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }

            match fs::read_to_string(&path) {
                Ok(json) => match serde_json::from_str::<StoredCapture>(&json) {
                    Ok(capture) => captures.push(CaptureInfo {
                        id: capture.id,
                        name: capture.name,
                        privacy: capture.privacy,
                        created_at: capture.created_at,
                        duration_ms: capture.metadata.duration_ms.unwrap_or(0),
                        event_count: capture.metadata.event_count,
                        tags: capture.metadata.tags,
                    }),
                    Err(e) => {
                        let w = format!("corrupt capture file {}: {}", path.display(), e);
                        tracing::warn!("{w}");
                        warnings.push(w);
                    }
                },
                Err(e) => {
                    let w = format!("unreadable capture file {}: {}", path.display(), e);
                    tracing::warn!("{w}");
                    warnings.push(w);
                }
            }
        }

        // Sort by creation time (newest first)
        captures.sort_by_key(|c| std::cmp::Reverse(c.created_at));

        Ok((captures, warnings))
    }

    /// Get the file path for a capture
    fn get_capture_path(&self, id: &Uuid) -> PathBuf {
        self.base_dir.join(format!("{}.json", id))
    }

    /// Export a capture to a specific path.
    ///
    /// Like [`save`](Self::save), this refuses to silently overwrite an
    /// existing file — it returns [`std::io::ErrorKind::AlreadyExists`] if
    /// `path` already exists, and writes atomically (temp + rename) so a failed
    /// export never leaves a partial/corrupt file at the user's path. Use
    /// [`export_overwrite`](Self::export_overwrite) to replace one deliberately.
    pub fn export(&self, id: &Uuid, path: &Path) -> Result<(), std::io::Error> {
        self.write_export(id, path, false)
    }

    /// Export a capture to a specific path, replacing any existing file.
    ///
    /// The explicit, opt-in counterpart to [`export`](Self::export);
    /// still writes atomically.
    pub fn export_overwrite(&self, id: &Uuid, path: &Path) -> Result<(), std::io::Error> {
        self.write_export(id, path, true)
    }

    fn write_export(&self, id: &Uuid, path: &Path, overwrite: bool) -> Result<(), std::io::Error> {
        let capture = self.load(id)?;
        let json = serde_json::to_string_pretty(&capture)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        atomic_write_json(path, &json, overwrite).map_err(|e| match e.kind() {
            std::io::ErrorKind::AlreadyExists => std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "export target {} already exists; use export_overwrite to replace it",
                    path.display()
                ),
            ),
            _ => e,
        })
    }

    /// Import a capture from a file
    pub fn import(&self, path: &Path, privacy: PrivacyLevel) -> Result<Uuid, std::io::Error> {
        let json = fs::read_to_string(path)?;
        let mut capture: StoredCapture = serde_json::from_str(&json)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

        // Generate new ID and update privacy
        capture.id = Uuid::new_v4();
        capture.privacy = privacy;
        capture.created_at = Utc::now();

        self.save(&capture)?;

        Ok(capture.id)
    }
}

/// Atomically write `json` to `target`: write the bytes fully to a temp file in
/// the same directory, then rename it into place. When `overwrite` is false the
/// rename is non-clobbering and returns [`std::io::ErrorKind::AlreadyExists`] if
/// `target` already exists.
///
/// The temp file is removed automatically on any failure, so `target`
/// only ever receives a complete file — never a partial/corrupt one, and an
/// existing file is never silently truncated. Shared by capture storage
/// ([`CaptureStorage::save`]) and export ([`CaptureStorage::export`]).
fn atomic_write_json(target: &Path, json: &str, overwrite: bool) -> Result<(), std::io::Error> {
    use std::io::Write;

    // The temp file must live in the same directory as `target` so the rename
    // is atomic (same filesystem). A bare filename has an empty parent → ".".
    let parent = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(json.as_bytes())?;
    tmp.flush()?;

    if overwrite {
        tmp.persist(target).map_err(|e| e.error)?;
    } else {
        tmp.persist_noclobber(target).map_err(|e| e.error)?;
    }
    Ok(())
}

/// Summary information about a capture
#[derive(Debug, Clone)]
pub struct CaptureInfo {
    pub id: Uuid,
    pub name: String,
    pub privacy: PrivacyLevel,
    pub created_at: DateTime<Utc>,
    pub duration_ms: u64,
    pub event_count: usize,
    pub tags: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_storage() -> (CaptureStorage, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let storage = CaptureStorage {
            base_dir: temp_dir.path().to_path_buf(),
        };
        (storage, temp_dir)
    }

    #[test]
    fn test_save_and_load() {
        let (storage, _temp) = create_test_storage();

        let capture = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "test-capture".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };

        // Save
        let path = storage.save(&capture).unwrap();
        assert!(path.exists());

        // Load
        let loaded = storage.load(&capture.id).unwrap();
        assert_eq!(loaded.id, capture.id);
        assert_eq!(loaded.name, capture.name);
    }

    /// `save` must not silently overwrite an existing capture file.
    /// A second `save` to the same id is rejected with `AlreadyExists` and the
    /// original file is left intact — the prior bug used `fs::write`, which
    /// truncated the existing capture without warning.
    #[test]
    fn save_does_not_silently_overwrite_existing_capture() {
        let (storage, _temp) = create_test_storage();

        let id = Uuid::new_v4();
        let original = StoredCapture {
            version: "3.1".to_string(),
            id,
            name: "original".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };
        storage.save(&original).unwrap();

        // A different capture that happens to reuse the id.
        let clobberer = StoredCapture {
            name: "clobberer".to_string(),
            ..original.clone()
        };
        let err = storage
            .save(&clobberer)
            .expect_err("saving over an existing capture file must error, not silently overwrite");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);

        // The original capture survives untouched.
        let loaded = storage.load(&id).unwrap();
        assert_eq!(loaded.name, "original");
    }

    /// `save_overwrite` is the explicit opt-in for replacing a capture
    /// in place — so callers that genuinely intend an update still can, but
    /// only deliberately.
    #[test]
    fn save_overwrite_replaces_existing_capture() {
        let (storage, _temp) = create_test_storage();

        let id = Uuid::new_v4();
        let original = StoredCapture {
            version: "3.1".to_string(),
            id,
            name: "original".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };
        storage.save(&original).unwrap();

        let updated = StoredCapture {
            name: "updated".to_string(),
            ..original.clone()
        };
        storage.save_overwrite(&updated).unwrap();

        let loaded = storage.load(&id).unwrap();
        assert_eq!(loaded.name, "updated");
    }

    /// (Copilot): the atomic temp-write + rename must not leave stray
    /// temp files behind on success — only the `{id}.json` capture should
    /// remain, so a later `list()` stays clean and no partial files linger.
    #[test]
    fn save_leaves_no_temp_files_behind() {
        let (storage, temp) = create_test_storage();

        let capture = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "c".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };
        storage.save(&capture).unwrap();
        storage.save_overwrite(&capture).unwrap();

        let entries: Vec<_> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries.len(), 1, "exactly one file expected: {entries:?}");
        assert!(
            entries[0].ends_with(".json"),
            "only the capture json should remain, no temp files: {entries:?}"
        );
    }

    /// `export` is "save a capture to a user path" and carried
    /// the same silent-clobber bug — it must not overwrite an existing file at
    /// the target either.
    #[test]
    fn export_does_not_silently_overwrite_existing_file() {
        let (storage, temp) = create_test_storage();

        let id = Uuid::new_v4();
        let capture = StoredCapture {
            version: "3.1".to_string(),
            id,
            name: "c".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };
        storage.save(&capture).unwrap();

        // A pre-existing file at the export target must not be clobbered.
        let target = temp.path().join("export.json");
        std::fs::write(&target, b"precious pre-existing data").unwrap();

        let err = storage
            .export(&id, &target)
            .expect_err("export over an existing file must error, not silently overwrite");
        assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "precious pre-existing data",
            "the pre-existing file must be left untouched"
        );

        // export_overwrite is the explicit opt-in.
        storage.export_overwrite(&id, &target).unwrap();
        let exported: StoredCapture =
            serde_json::from_str(&std::fs::read_to_string(&target).unwrap()).unwrap();
        assert_eq!(exported.id, id);
    }

    /// Export to a fresh path succeeds and leaves no stray temp files.
    #[test]
    fn export_to_new_path_writes_atomically() {
        let (storage, temp) = create_test_storage();

        let id = Uuid::new_v4();
        let capture = StoredCapture {
            version: "3.1".to_string(),
            id,
            name: "c".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };
        storage.save(&capture).unwrap();

        let out_dir = temp.path().join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let target = out_dir.join("export.json");
        storage.export(&id, &target).unwrap();

        assert!(target.exists());
        let entries: Vec<_> = std::fs::read_dir(&out_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            entries,
            vec!["export.json".to_string()],
            "only the exported file should remain, no temp files: {entries:?}"
        );
    }

    #[test]
    fn test_list() {
        let (storage, _temp) = create_test_storage();

        // Create multiple captures
        for i in 0..3 {
            let capture = StoredCapture {
                version: "3.1".to_string(),
                id: Uuid::new_v4(),
                name: format!("capture-{}", i),
                privacy: PrivacyLevel::Private,
                created_at: Utc::now(),
                metadata: CaptureMetadata::default(),
                events: Vec::new(),
            };
            storage.save(&capture).unwrap();
        }

        let list = storage.list().unwrap();
        assert_eq!(list.len(), 3);
    }

    #[test]
    fn test_delete() {
        let (storage, _temp) = create_test_storage();

        let capture = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "test-capture".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };

        storage.save(&capture).unwrap();
        storage.delete(&capture.id).unwrap();

        assert!(storage.load(&capture.id).is_err());
    }

    /// Regression test. A corrupt `.json` in the captures directory must be
    /// reported (via `list_with_warnings`) rather than silently omitted —
    /// previously `list()` swallowed the parse error and the file vanished,
    /// hiding the data loss.
    #[test]
    fn list_reports_corrupt_capture_files() {
        let (storage, temp) = create_test_storage();

        // One valid capture.
        let good = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "good".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };
        storage.save(&good).unwrap();

        // One corrupt `.json` file alongside it.
        std::fs::write(temp.path().join("corrupt.json"), b"{ not valid json").unwrap();

        // list() still returns the readable capture (corrupt one omitted)...
        let captures = storage.list().unwrap();
        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].name, "good");

        // ...but list_with_warnings surfaces the corruption instead of hiding it.
        let (captures2, warnings) = storage.list_with_warnings().unwrap();
        assert_eq!(captures2.len(), 1);
        assert_eq!(
            warnings.len(),
            1,
            "the corrupt file must be reported: {warnings:?}"
        );
        assert!(
            warnings[0].contains("corrupt.json"),
            "warning must name the offending file: {warnings:?}"
        );
    }

    /// Companion test: the *unreadable* branch (read error, distinct from a
    /// parse error) must also be surfaced. A directory whose name ends in
    /// `.json` passes the extension filter but fails `fs::read_to_string`,
    /// exercising that arm.
    #[test]
    fn list_reports_unreadable_capture_files() {
        let (storage, temp) = create_test_storage();

        // One valid capture.
        let good = StoredCapture {
            version: "3.1".to_string(),
            id: Uuid::new_v4(),
            name: "good".to_string(),
            privacy: PrivacyLevel::Private,
            created_at: Utc::now(),
            metadata: CaptureMetadata::default(),
            events: Vec::new(),
        };
        storage.save(&good).unwrap();

        // A directory ending in `.json`: read_to_string fails on it.
        std::fs::create_dir(temp.path().join("unreadable.json")).unwrap();

        let (captures, warnings) = storage.list_with_warnings().unwrap();
        assert_eq!(captures.len(), 1, "the valid capture is still listed");
        assert_eq!(captures[0].name, "good");
        assert_eq!(
            warnings.len(),
            1,
            "the unreadable entry must be reported: {warnings:?}"
        );
        assert!(
            warnings[0].contains("unreadable capture file"),
            "warning must use the unreadable wording: {warnings:?}"
        );
        assert!(
            warnings[0].contains("unreadable.json"),
            "warning must name the offending path: {warnings:?}"
        );
    }
}
