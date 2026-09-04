// Copyright 2025-2026 Monstrous Media
// SPDX-License-Identifier: MIT

//! ADR-034 §D2.2 — safe-walk path validation for `ReloadFromDisk` /
//! `ImportConfig`.
//!
//! R3's "`O_NOFOLLOW` on open" guards only the *final* path component. A
//! symlinked **ancestor** — e.g. the config directory itself being a symlink
//! to `/tmp/attacker/` — defeats that check (R4-M3). This module resolves a
//! caller-supplied config path **beneath a fixed allowlist root** without ever
//! following a symlink in any component, and **without** calling
//! `canonicalize` (which resolves *through* symlinks — the very surface we are
//! closing).
//!
//! - **Linux:** `openat2(root, rel, RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS)` on
//!   kernel 5.6+, falling back to the iterative per-component walk below when
//!   `openat2` is unavailable (old kernel → `ENOSYS`; seccomp → `EPERM`).
//! - **macOS / fallback:** iterative `openat(dirfd, component,
//!   O_RDONLY | O_NOFOLLOW)` for each component (no `O_DIRECTORY`, so a
//!   symlinked component fails with `ELOOP` rather than being masked as
//!   `ENOTDIR`), `fstat`-verifying each intermediate is a directory before
//!   descending. Rejects a symlink at any level. macOS lacks `openat2`; this is
//!   the portable equivalent.
//!
//! On success the **already-open** file descriptor is returned (or read from
//! directly), so the caller never re-opens by path — closing the TOCTOU window
//! between validation and read.
//!
//! Lexical pre-checks (no `..`, must end `.toml`, must be absolute) and a
//! post-open `fstat` (regular-file mode + owning UID) bracket the walk. All
//! opens carry `O_NONBLOCK` so a FIFO/special-file component cannot wedge the
//! daemon thread (it is then rejected as non-regular by the `fstat`).
//!
//! **Threat-model scope (ADR-034 §D3.3):** this defends against a *caller*
//! supplying a hostile `target` path. It does NOT defend against a **same-UID**
//! attacker who can replace an *ancestor of the allowlist root itself* (e.g.
//! symlinking the daemon's config-dir parent) — that is explicitly out of the
//! ADR-034 threat model (audit is the detection control). The spec's
//! `CONFIG_DIR_FD`-captured-at-startup mitigation for ancestor swaps is a
//! deferred follow-up; `open_root` re-validates the root's final
//! component (`O_NOFOLLOW`) on every call in the meantime.

use std::ffi::OsStr;
use std::fs::File;
use std::os::fd::OwnedFd;
use std::path::{Component, Path};

use std::os::unix::fs::MetadataExt;

use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::sys::stat::Mode;

/// Why a caller-supplied config path was rejected by the §D2.2 safe-walk.
///
/// Deliberately coarse and `Copy`/`Eq` so call sites and tests can match on
/// the discriminant without threading an `io::Error` around; the human-facing
/// detail (errno text, the expected/found UID) rides alongside in
/// [`PathValidationError::detail`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathRejectReason {
    /// Path was not absolute.
    NotAbsolute,
    /// Path contained a `..` component.
    ParentTraversal,
    /// Path did not end in `.toml`.
    NotToml,
    /// Path did not lie beneath the allowlist root.
    NotBeneathRoot,
    /// The relative portion (target minus root) was empty.
    EmptyRelativePath,
    /// The allowlist root itself could not be opened as a non-symlinked
    /// directory (it does not exist, is not a directory, or *is* a symlink).
    RootRejected,
    /// A symlink was encountered in some component of the walk
    /// (`ELOOP` / `RESOLVE_NO_SYMLINKS`), or the path escaped the root
    /// (`RESOLVE_BENEATH`).
    SymlinkInPath,
    /// A component of the path does not exist (`ENOENT`). Surfaced distinctly so
    /// callers can map it to `ConfigNotFound` (operator typo) rather than a
    /// security rejection — it is not audited as a validation failure.
    TargetNotFound,
    /// A component could not be traversed (`ENOTDIR` — a non-directory used as a
    /// path component — or permission denied).
    ComponentUnreadable,
    /// The final target was not a regular file (e.g. a directory or FIFO).
    NotRegularFile,
    /// The final target was not owned by the expected (daemon) UID.
    OwnerMismatch,
}

impl PathRejectReason {
    /// Stable snake_case discriminator for the `PathValidationFailed` audit
    /// event and structured logs.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotAbsolute => "not_absolute",
            Self::ParentTraversal => "parent_traversal",
            Self::NotToml => "not_toml",
            Self::NotBeneathRoot => "not_beneath_root",
            Self::EmptyRelativePath => "empty_relative_path",
            Self::RootRejected => "root_rejected",
            Self::SymlinkInPath => "symlink_in_path",
            Self::TargetNotFound => "target_not_found",
            Self::ComponentUnreadable => "component_unreadable",
            Self::NotRegularFile => "not_regular_file",
            Self::OwnerMismatch => "owner_mismatch",
        }
    }
}

/// A rejection from the §D2.2 safe-walk, carrying the coarse reason plus an
/// optional human detail (never the attacker-controlled path — call sites that
/// want to record the attempted path do so explicitly).
#[derive(Debug, Clone)]
pub struct PathValidationError {
    reason: PathRejectReason,
    detail: Option<String>,
}

impl PathValidationError {
    fn bare(reason: PathRejectReason) -> Self {
        Self {
            reason,
            detail: None,
        }
    }

    fn detailed(reason: PathRejectReason, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: Some(detail.into()),
        }
    }

    /// The coarse rejection discriminant (for matching / metrics / tests).
    pub fn reason_code(&self) -> PathRejectReason {
        self.reason
    }

    /// Operator-facing reason string for the audit event and IPC error message.
    pub fn reason(&self) -> String {
        match &self.detail {
            Some(d) => format!("{}: {d}", self.reason.as_str()),
            None => self.reason.as_str().to_string(),
        }
    }
}

impl std::fmt::Display for PathValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason())
    }
}

impl std::error::Error for PathValidationError {}

/// Failure of [`read_config_to_string`]: either the path was rejected by the
/// safe-walk, or the (validated) file could not be read.
#[derive(Debug)]
pub enum SafeReadError {
    /// The path failed §D2.2 validation — maps to
    /// `IpcErrorCode::PathValidationFailed`, except
    /// [`PathRejectReason::TargetNotFound`] which the caller maps to
    /// `ConfigNotFound` (a missing path is an operator typo, not an attack).
    Validation(PathValidationError),
    /// The validated fd could not be read. The open already succeeded, so a
    /// `NotFound` here is effectively unreachable (a missing path surfaces as
    /// `Validation(TargetNotFound)` during the walk); this covers mid-read I/O
    /// faults → `InternalError`.
    Read(std::io::Error),
}

/// Lexical pre-checks shared by both platforms: absolute, no `..`, ends
/// `.toml`. Mirrors [`crate::daemon::engine_manager::helpers::lexical_config_path_ok`]
/// but returns the structured reason so the safe-walk can audit it uniformly.
fn lexical_ok(target: &Path) -> Result<(), PathValidationError> {
    if !target.is_absolute() {
        return Err(PathValidationError::bare(PathRejectReason::NotAbsolute));
    }
    if target
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(PathValidationError::bare(PathRejectReason::ParentTraversal));
    }
    if target.extension().and_then(OsStr::to_str) != Some("toml") {
        return Err(PathValidationError::bare(PathRejectReason::NotToml));
    }
    Ok(())
}

/// The normalised, root-relative `Normal` components of `target` beneath
/// `root`. Rejects targets not lexically beneath `root`. `CurDir` (`.`)
/// components are skipped; `ParentDir` was already rejected lexically.
fn relative_components<'a>(
    root: &Path,
    target: &'a Path,
) -> Result<Vec<&'a OsStr>, PathValidationError> {
    let rel = target
        .strip_prefix(root)
        .map_err(|_| PathValidationError::bare(PathRejectReason::NotBeneathRoot))?;

    let mut comps = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(s) => comps.push(s),
            Component::CurDir => {}
            // strip_prefix already removed any RootDir/Prefix; a ParentDir
            // here would have been caught lexically, but reject defensively.
            _ => return Err(PathValidationError::bare(PathRejectReason::ParentTraversal)),
        }
    }
    if comps.is_empty() {
        return Err(PathValidationError::bare(
            PathRejectReason::EmptyRelativePath,
        ));
    }
    Ok(comps)
}

/// Open the allowlist root as a directory **without** following a final
/// symlink (`O_NOFOLLOW`). If the root is itself a symlink at this instant the
/// open fails with `ELOOP` → [`PathRejectReason::RootRejected`], matching the
/// spec's "refuse if the root is a symlink".
fn open_root(root: &Path) -> Result<OwnedFd, PathValidationError> {
    // `O_NONBLOCK`: a FIFO/special-file component must never block the open (an
    // attacker with write access to the config tree could otherwise wedge the
    // daemon thread). It is a no-op for directories and regular files.
    let flags = OFlag::O_RDONLY
        | OFlag::O_DIRECTORY
        | OFlag::O_NOFOLLOW
        | OFlag::O_CLOEXEC
        | OFlag::O_NONBLOCK;
    nix::fcntl::open(root, flags, Mode::empty())
        .map_err(|e| PathValidationError::detailed(PathRejectReason::RootRejected, e.to_string()))
}

/// Map an `openat`/`openat2` errno from the walk to a rejection reason.
fn map_walk_errno(e: Errno) -> PathValidationError {
    match e {
        // O_NOFOLLOW hit a symlink, or RESOLVE_NO_SYMLINKS tripped.
        Errno::ELOOP => PathValidationError::bare(PathRejectReason::SymlinkInPath),
        // RESOLVE_BENEATH: the path tried to escape the root.
        Errno::EXDEV => PathValidationError::bare(PathRejectReason::NotBeneathRoot),
        // A missing component is an operator typo, not an attack — let the
        // caller map it to ConfigNotFound (preserves pre-safe-walk semantics).
        Errno::ENOENT => PathValidationError::bare(PathRejectReason::TargetNotFound),
        Errno::ENOTDIR | Errno::EACCES => {
            PathValidationError::detailed(PathRejectReason::ComponentUnreadable, e.to_string())
        }
        other => {
            PathValidationError::detailed(PathRejectReason::ComponentUnreadable, other.to_string())
        }
    }
}

/// Portable per-component walk: each intermediate component is opened with
/// `O_NOFOLLOW` (so a symlink reliably fails `ELOOP` rather than being masked by
/// an `O_DIRECTORY` `ENOTDIR`), then `fstat`-verified (via `File::metadata`, an
/// `fstat` on the fd — no second path lookup) to be a directory before
/// descending. The final component is opened `O_NOFOLLOW` as a file. Used
/// directly on macOS and as the `openat2` fallback on Linux.
fn iterative_open(dir_fd: OwnedFd, comps: &[&OsStr]) -> Result<File, PathValidationError> {
    let (last, intermediates) = comps
        .split_last()
        .ok_or_else(|| PathValidationError::bare(PathRejectReason::EmptyRelativePath))?;

    // `O_NONBLOCK`: never block the open on a FIFO/special-file component (the
    // final `fstat` then rejects it as non-regular). No-op for dirs/regular
    // files. `O_NOFOLLOW` (no `O_DIRECTORY`) keeps symlinks failing `ELOOP`.
    let nofollow = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK;
    let mut dir = File::from(dir_fd);
    for c in intermediates {
        // `O_NOFOLLOW` without `O_DIRECTORY`: a symlinked component fails with
        // `ELOOP` (→ SymlinkInPath), unambiguously, on both Linux and macOS.
        let next = nix::fcntl::openat(&dir, Path::new(c), nofollow, Mode::empty())
            .map_err(map_walk_errno)?;
        let next = File::from(next);
        let md = next.metadata().map_err(|e| {
            PathValidationError::detailed(PathRejectReason::ComponentUnreadable, e.to_string())
        })?;
        if !md.is_dir() {
            // A non-symlinked, non-directory intermediate (e.g. a regular file
            // used as a path component).
            return Err(PathValidationError::bare(
                PathRejectReason::ComponentUnreadable,
            ));
        }
        dir = next;
    }

    let file_fd = nix::fcntl::openat(&dir, Path::new(last), nofollow, Mode::empty())
        .map_err(map_walk_errno)?;
    Ok(File::from(file_fd))
}

/// Linux fast path: a single `openat2` with `RESOLVE_BENEATH |
/// RESOLVE_NO_SYMLINKS` resolves the whole relative path atomically. Falls back
/// to [`iterative_open`] when the syscall is unavailable.
#[cfg(target_os = "linux")]
fn safe_open(root: &Path, comps: &[&OsStr]) -> Result<File, PathValidationError> {
    use nix::fcntl::{OpenHow, ResolveFlag, openat2};

    let root_fd = open_root(root)?;
    let mut rel = std::path::PathBuf::new();
    for c in comps {
        rel.push(c);
    }

    // `O_NONBLOCK`: don't block on a FIFO/special-file target (rejected by the
    // post-open fstat). `RESOLVE_NO_SYMLINKS` covers every component including
    // the final one, so a separate `O_NOFOLLOW` is redundant here.
    let how = OpenHow::new()
        .flags(OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK)
        .resolve(ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_SYMLINKS);

    match openat2(&root_fd, &rel, how) {
        Ok(fd) => Ok(File::from(fd)),
        // openat2 unsupported: old kernel (<5.6) or blocked by seccomp.
        Err(Errno::ENOSYS) | Err(Errno::EPERM) => iterative_open(root_fd, comps),
        Err(e) => Err(map_walk_errno(e)),
    }
}

#[cfg(not(target_os = "linux"))]
fn safe_open(root: &Path, comps: &[&OsStr]) -> Result<File, PathValidationError> {
    let root_fd = open_root(root)?;
    iterative_open(root_fd, comps)
}

/// `fstat` the opened target: it must be a regular file owned by
/// `expected_uid`. The check is on the **opened fd**, so it describes the exact
/// object the read will consume — no second path lookup.
fn verify_fstat(file: &File, expected_uid: u32) -> Result<(), PathValidationError> {
    // `File::metadata` is an `fstat` on the open fd — it describes the exact
    // object the read will consume, with no second path lookup.
    let md = file.metadata().map_err(|e| {
        PathValidationError::detailed(PathRejectReason::ComponentUnreadable, e.to_string())
    })?;

    if !md.file_type().is_file() {
        return Err(PathValidationError::bare(PathRejectReason::NotRegularFile));
    }

    if md.uid() != expected_uid {
        return Err(PathValidationError::detailed(
            PathRejectReason::OwnerMismatch,
            format!("expected uid {expected_uid}, found {}", md.uid()),
        ));
    }
    Ok(())
}

/// Safely open `target` for reading, guaranteeing it resolves to a regular
/// file beneath `root`, owned by `expected_uid`, **without** following any
/// symlink in any path component and **without** `canonicalize`. On success the
/// returned [`File`] is the validated fd — read from it directly; do not
/// re-open by path.
///
/// `expected_uid` is normally the daemon's effective UID
/// (`nix::unistd::geteuid()`).
pub fn open_config_beneath(
    root: &Path,
    target: &Path,
    expected_uid: u32,
) -> Result<File, PathValidationError> {
    lexical_ok(target)?;
    let comps = relative_components(root, target)?;
    let file = safe_open(root, &comps)?;
    verify_fstat(&file, expected_uid)?;
    Ok(file)
}

/// [`open_config_beneath`] followed by a full read of the validated fd into a
/// `String`. The read is from the already-validated descriptor, so a symlink
/// swap between validation and read cannot redirect it (no TOCTOU).
///
/// This performs **blocking** syscalls (`open`/`openat`/`read`) and must be
/// called from a blocking context (e.g. `tokio::task::spawn_blocking`).
pub fn read_config_to_string(
    root: &Path,
    target: &Path,
    expected_uid: u32,
) -> Result<String, SafeReadError> {
    use std::io::Read;
    let mut file =
        open_config_beneath(root, target, expected_uid).map_err(SafeReadError::Validation)?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).map_err(SafeReadError::Read)?;
    Ok(buf)
}

/// [`read_config_to_string`] with the allowlist root derived as the parent
/// directory of `config_path` **inside this call**. Co-locating the root
/// derivation with the safe-walk removes any check-then-use window on the root:
/// the parent is computed and the root is opened (`O_NOFOLLOW`) in the same
/// blocking context, immediately before the walk. A `config_path` with no
/// parent (e.g. a bare filename) is itself a validation failure.
///
/// Like [`read_config_to_string`], this performs **blocking** syscalls and must
/// run on a blocking worker.
pub fn read_config_beneath_config_dir(
    config_path: &Path,
    target: &Path,
    expected_uid: u32,
) -> Result<String, SafeReadError> {
    let root = config_path.parent().ok_or_else(|| {
        SafeReadError::Validation(PathValidationError::detailed(
            PathRejectReason::RootRejected,
            "config path has no parent directory",
        ))
    })?;
    read_config_to_string(root, target, expected_uid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    fn uid() -> u32 {
        // SAFETY: geteuid is always successful and has no preconditions.
        unsafe { libc::geteuid() }
    }

    /// Build a temp dir, write a valid-looking config file in it, return both.
    fn root_with_config(name: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::write(&p, "x = 1\n").unwrap();
        (dir, p)
    }

    #[test]
    fn opens_a_regular_toml_beneath_root() {
        let (dir, p) = root_with_config("live.toml");
        let f = open_config_beneath(dir.path(), &p, uid()).expect("valid path opens");
        // The returned fd is the validated file; reading it yields its content.
        use std::io::Read;
        let mut s = String::new();
        (&f).read_to_string(&mut s).unwrap();
        assert_eq!(s, "x = 1\n");
    }

    #[test]
    fn opens_a_toml_in_a_real_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("profiles");
        std::fs::create_dir(&sub).unwrap();
        let p = sub.join("studio.toml");
        std::fs::write(&p, "y = 2\n").unwrap();
        let f = open_config_beneath(dir.path(), &p, uid()).expect("nested real dir opens");
        drop(f);
    }

    #[test]
    fn rejects_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let err = open_config_beneath(dir.path(), Path::new("live.toml"), uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::NotAbsolute);
    }

    #[test]
    fn rejects_parent_traversal() {
        let dir = tempfile::tempdir().unwrap();
        let escaped = dir.path().join("../live.toml");
        let err = open_config_beneath(dir.path(), &escaped, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::ParentTraversal);
    }

    #[test]
    fn rejects_non_toml_extension() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("live.json");
        std::fs::write(&p, "{}").unwrap();
        let err = open_config_beneath(dir.path(), &p, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::NotToml);
    }

    #[test]
    fn rejects_path_not_beneath_root() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let p = other.path().join("live.toml");
        std::fs::write(&p, "z = 3\n").unwrap();
        let err = open_config_beneath(root.path(), &p, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::NotBeneathRoot);
    }

    /// Final-component symlink: `live.toml` is a symlink (even to a benign file
    /// beneath the root). `O_NOFOLLOW` on the final open must reject it.
    #[test]
    fn rejects_final_component_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.toml");
        std::fs::write(&real, "ok = 1\n").unwrap();
        let link = dir.path().join("live.toml");
        symlink(&real, &link).unwrap();
        let err = open_config_beneath(dir.path(), &link, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::SymlinkInPath);
    }

    /// Parent-component symlink: an intermediate directory is a symlink (the
    /// `~/.config/conductor -> /tmp/attacker` attack). The per-component
    /// `O_NOFOLLOW` walk must reject it even though the final component is a
    /// real file.
    #[test]
    fn rejects_parent_component_symlink() {
        let dir = tempfile::tempdir().unwrap();
        // A real directory holding the actual file…
        let real_dir = dir.path().join("real_profiles");
        std::fs::create_dir(&real_dir).unwrap();
        std::fs::write(real_dir.join("studio.toml"), "p = 1\n").unwrap();
        // …and a symlink pointing at it, used as the intermediate component.
        let link_dir = dir.path().join("profiles");
        symlink(&real_dir, &link_dir).unwrap();
        let via_link = link_dir.join("studio.toml");
        let err = open_config_beneath(dir.path(), &via_link, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::SymlinkInPath);
    }

    /// Absolute symlink escape: a symlink beneath the root pointing OUTSIDE it
    /// (`/etc/...`) must not let the resolution escape. `O_NOFOLLOW` rejects the
    /// symlink itself (the resolution never gets the chance to leave the root).
    #[test]
    fn rejects_symlink_escaping_root() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let link = dir.path().join("escape.toml");
        symlink(outside.path(), &link).unwrap();
        let err = open_config_beneath(dir.path(), &link, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::SymlinkInPath);
    }

    /// Inode-ownership mismatch: a file owned by another UID is rejected even
    /// when it is a real regular file beneath the root. We cannot `chown` to a
    /// foreign UID without privilege, so we simulate by asserting against a
    /// deliberately-wrong `expected_uid`.
    #[test]
    fn rejects_owner_mismatch() {
        let (dir, p) = root_with_config("live.toml");
        let wrong_uid = uid().wrapping_add(1);
        let err = open_config_beneath(dir.path(), &p, wrong_uid).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::OwnerMismatch);
    }

    /// A directory whose name ends in `.toml` passes the lexical check but is
    /// not a regular file → `NotRegularFile`.
    /// A missing target beneath a real root is `TargetNotFound` (→ the caller
    /// maps it to ConfigNotFound), not a security rejection.
    #[test]
    fn missing_target_is_target_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("nope.toml");
        let err = open_config_beneath(dir.path(), &missing, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::TargetNotFound);
    }

    #[test]
    fn rejects_directory_named_toml() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("config.toml");
        std::fs::create_dir(&bogus).unwrap();
        let err = open_config_beneath(dir.path(), &bogus, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::NotRegularFile);
    }

    /// A FIFO named `*.toml` beneath the root is rejected as non-regular
    /// **without blocking** the open (the `O_NONBLOCK` guard). Without
    /// `O_NONBLOCK`, `open(O_RDONLY)` on a writer-less FIFO would hang and this
    /// test would deadlock instead of failing fast.
    #[test]
    fn rejects_fifo_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let fifo = dir.path().join("pipe.toml");
        nix::unistd::mkfifo(
            &fifo,
            nix::sys::stat::Mode::S_IRUSR | nix::sys::stat::Mode::S_IWUSR,
        )
        .unwrap();
        // No writer is ever opened — a blocking O_RDONLY open would deadlock.
        let err = open_config_beneath(dir.path(), &fifo, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::NotRegularFile);
    }

    /// A symlinked **root** is refused (`open_root` uses `O_NOFOLLOW`), modelling
    /// the spec's "if the root itself is a symlink, refuse".
    #[test]
    fn rejects_symlinked_root() {
        let real_root = tempfile::tempdir().unwrap();
        std::fs::write(real_root.path().join("live.toml"), "a = 1\n").unwrap();
        let parent = tempfile::tempdir().unwrap();
        let link_root = parent.path().join("conductor");
        symlink(real_root.path(), &link_root).unwrap();
        // Target is lexically beneath the (symlinked) root.
        let target = link_root.join("live.toml");
        let err = open_config_beneath(&link_root, &target, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::RootRejected);
    }

    /// Runtime allowlist-root replacement: the same target resolves under
    /// whichever root is supplied. Passing a *different* root rejects a path
    /// that was valid under the first — proving the function is rooted at its
    /// argument, not a frozen global.
    #[test]
    fn allowlist_root_is_per_call() {
        let (dir_a, p_a) = root_with_config("live.toml");
        let dir_b = tempfile::tempdir().unwrap();
        // Valid under root A…
        open_config_beneath(dir_a.path(), &p_a, uid()).expect("valid under its own root");
        // …rejected under root B (not beneath it).
        let err = open_config_beneath(dir_b.path(), &p_a, uid()).unwrap_err();
        assert_eq!(err.reason_code(), PathRejectReason::NotBeneathRoot);
    }

    #[test]
    fn read_config_to_string_reads_validated_fd() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("live.toml");
        std::fs::write(&p, "hello = \"world\"\n").unwrap();
        let s = read_config_to_string(dir.path(), &p, uid()).expect("reads");
        assert_eq!(s, "hello = \"world\"\n");
    }

    #[test]
    fn read_config_beneath_config_dir_derives_parent_root() {
        // config_path = <dir>/live.toml → root = <dir>; a sibling .toml beneath
        // it reads, a path outside it is rejected.
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("live.toml");
        let sibling = dir.path().join("import.toml");
        std::fs::write(&sibling, "a = 1\n").unwrap();
        let s =
            read_config_beneath_config_dir(&config_path, &sibling, uid()).expect("reads sibling");
        assert_eq!(s, "a = 1\n");

        let outside = tempfile::tempdir().unwrap();
        let outside_file = outside.path().join("import.toml");
        std::fs::write(&outside_file, "b = 2\n").unwrap();
        match read_config_beneath_config_dir(&config_path, &outside_file, uid()) {
            Err(SafeReadError::Validation(e)) => {
                assert_eq!(e.reason_code(), PathRejectReason::NotBeneathRoot);
            }
            other => panic!("expected NotBeneathRoot, got {other:?}"),
        }
    }

    #[test]
    fn read_config_beneath_config_dir_rejects_parentless_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("x.toml");
        std::fs::write(&target, "k = 1\n").unwrap();
        // "/" has no parent.
        match read_config_beneath_config_dir(Path::new("/"), &target, uid()) {
            Err(SafeReadError::Validation(e)) => {
                assert_eq!(e.reason_code(), PathRejectReason::RootRejected);
            }
            other => panic!("expected RootRejected, got {other:?}"),
        }
    }

    #[test]
    fn read_config_to_string_surfaces_validation_error() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("live.toml");
        let real = dir.path().join("real.toml");
        std::fs::write(&real, "k = 1\n").unwrap();
        symlink(&real, &link).unwrap();
        match read_config_to_string(dir.path(), &link, uid()) {
            Err(SafeReadError::Validation(e)) => {
                assert_eq!(e.reason_code(), PathRejectReason::SymlinkInPath);
            }
            other => panic!("expected Validation error, got {other:?}"),
        }
    }

    #[test]
    fn reason_string_includes_discriminator() {
        let e = PathValidationError::bare(PathRejectReason::SymlinkInPath);
        assert_eq!(e.reason(), "symlink_in_path");
        let e2 = PathValidationError::detailed(
            PathRejectReason::OwnerMismatch,
            "expected uid 1, found 2",
        );
        assert_eq!(e2.reason(), "owner_mismatch: expected uid 1, found 2");
    }
}
