//! Cross-process advisory locking for `config.toml`'s read-modify-write
//! cycle.
//!
//! Both the CLI (`supply-drop-bbs config <subcommand>`, `node set-radio`,
//! etc.) and the web admin's config-mutating endpoints do the same
//! unprotected sequence: read `config.toml`, parse it, apply an edit in
//! memory, then atomically rewrite it (temp file + `sync_all` + `rename`).
//! Nothing serializes concurrent writers — two CLI invocations, or a CLI
//! invocation racing the running server's web admin (a *different OS
//! process*, so an in-process `Mutex` alone can't help).
//!
//! Two real failure modes without a lock (see supply-drop-bbs / #227):
//!
//! 1. **Lost update / validation bypass** — two concurrent writers each
//!    read a pre-write snapshot, each independently pass their own
//!    validation against that stale snapshot, then both write. The loser's
//!    write silently clobbers the winner's, and/or the two checks can each
//!    pass against a state the other's subsequent write invalidates —
//!    concretely, this can defeat the `[bbs].name` /
//!    `[location].share_in_advert` byte-budget cross-check
//!    (`bbs_core::mesh_name`), landing on the exact forbidden combination
//!    neither individual check saw.
//! 2. **Temp-file collision** — the atomic-write helpers (in `src/main.rs`
//!    and `bbs-web`) always write to a fixed `<path>.tmp` sibling. Two
//!    concurrent writers racing that same fixed name can interleave, and a
//!    corrupted `.tmp` can get renamed over the real `config.toml`. A lock
//!    held across the whole read-modify-write-rename cycle rules this out
//!    by construction (only one writer is ever inside the critical section
//!    at a time) — no unique temp-filename scheme is needed on top.
//!
//! ## Usage
//!
//! Wrap the existing read/parse/modify/atomic-write body in a closure and
//! pass it to [`with_config_lock`] (async contexts, e.g. `bbs-web` handlers
//! and `bbs-core`'s `BbsHost` methods) or [`with_config_lock_sync`] (the
//! CLI). Every config-mutating call site — in `src/main.rs`, `bbs-web`, and
//! `bbs-core` alike — must go through one of these; a call site that
//! mutates `config.toml` without holding this lock reopens the race for
//! everyone else too, not just itself. There is no compiler-enforced way to
//! catch a missed call site — when adding one, grep for existing
//! `with_config_lock`/`with_config_lock_sync` callers first.
//!
//! ## Implementation
//!
//! Locks a *separate*, stable sidecar file (`<config_path>.lock`) via
//! `fd-lock` (a safe wrapper around `flock(2)`/`LockFileEx` — this
//! workspace forbids `unsafe_code`, so a raw libc FFI call isn't an
//! option), rather than locking `config.toml` itself:
//! `atomic_write_file`'s rename-over-destination pattern replaces
//! `config.toml`'s inode on every write, so a lock held on the file being
//! replaced would not protect the file that exists after the rename.
//! `<path>.lock` is only ever opened and locked, never replaced, so every
//! writer locks the same, stable inode.
//!
//! `config_path` is canonicalized before deriving the sidecar path, so that
//! two callers spelling the same file differently (relative vs. absolute,
//! different working directories — exactly how the CLI and the running
//! server resolve their config path today) still contend on the same lock.
//! The sidecar is opened with `O_NOFOLLOW` so a pre-planted symlink at
//! `<config_path>.lock` can't redirect a locked write's truncation onto an
//! arbitrary file.
//!
//! `flock(2)`'s guarantees assume a local filesystem. Over NFS, whether a
//! lock is even visible to other hosts depends on mount options (e.g. a
//! `local_lock=flock`/`local_lock=all` mount makes locks purely
//! client-local, silently defeating cross-host exclusion). This project
//! targets local-disk, single-host deployments (a Pi or similar); a
//! networked `config.toml` is out of scope.

use std::fs::OpenOptions;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// Derive the sidecar lock path for `config_path`, canonicalizing first so
/// that two callers referring to the same file through different spellings
/// (relative vs. absolute, different cwd) contend on the same lock. Falls
/// back to the given path unchanged if canonicalization fails (e.g.
/// `config_path` doesn't exist yet, on a first-ever run) — the sidecar path
/// derived from it is then only as stable as the caller's own spelling,
/// same as before this fallback existed.
fn lock_file_path(config_path: &Path) -> PathBuf {
    let canonical = config_path
        .canonicalize()
        .unwrap_or_else(|_| config_path.to_path_buf());
    let mut s = canonical.as_os_str().to_owned();
    s.push(".lock");
    PathBuf::from(s)
}

/// Acquire the exclusive config-file lock and run `f` while holding it,
/// blocking the calling thread until the lock is available.
///
/// `f`'s `Err` is returned unchanged, including its [`io::ErrorKind`] —
/// callers may match on the kind to distinguish e.g. a validation failure
/// from an I/O failure. A panic inside `f` unwinds through this function
/// normally (the lock guard's `Drop` still runs, releasing the lock) unless
/// the binary is built with `panic = "abort"` (as this workspace's
/// `release-min` profile is), in which case the whole process aborts and
/// the OS releases the lock by closing the fd.
///
/// Synchronous — call directly from the CLI's `main` thread. From an
/// async/tokio context (the web admin's handlers, `bbs-core`'s `BbsHost`
/// methods), use [`with_config_lock`] instead, which runs this on a
/// blocking-pool thread so a tokio worker isn't blocked either waiting out
/// lock contention or doing `f`'s synchronous file I/O — calling this
/// synchronous version directly from an async handler blocks that task's
/// worker thread for the duration.
pub fn with_config_lock_sync<T>(
    config_path: &Path,
    f: impl FnOnce() -> io::Result<T>,
) -> io::Result<T> {
    let lock_path = lock_file_path(config_path);
    // Content is never read or written to this file — only its existence
    // and the lock held on its descriptor matter. `create(true)` truncates
    // if it already exists (from a prior run); that's fine, it's always
    // empty. `O_NOFOLLOW` refuses to open through a symlink: without it, a
    // local process with write access to the config directory could
    // pre-plant `<config_path>.lock` as a symlink to an arbitrary file this
    // process can write, and every locked config write would silently
    // truncate that file instead.
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)
        .map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "could not open config lock file {}: {e}",
                    lock_path.display()
                ),
            )
        })?;
    let mut rw = fd_lock::RwLock::new(file);
    // Blocks until acquired. Released when `_guard` drops at the end of
    // this function, regardless of whether `f` returns Ok or Err.
    let _guard = rw.write()?;
    f()
}

/// Async equivalent of [`with_config_lock_sync`], for async callers (the web
/// admin's handlers, `bbs-core`'s `BbsHost` methods).
///
/// Unlike the sync version, a panic inside `f` does *not* unwind into the
/// caller — `spawn_blocking` catches it and this function returns it as an
/// ordinary `Err` instead (still distinct from `f`'s own `Err`s only by
/// message text, not by `ErrorKind`).
pub async fn with_config_lock<T, F>(config_path: &Path, f: F) -> io::Result<T>
where
    F: FnOnce() -> io::Result<T> + Send + 'static,
    T: Send + 'static,
{
    let path = config_path.to_path_buf();
    tokio::task::spawn_blocking(move || with_config_lock_sync(&path, f))
        .await
        .unwrap_or_else(|join_err| {
            // `JoinError` covers both a panic inside `f` and the blocking
            // task being cancelled (e.g. the runtime shutting down) —
            // don't claim "panicked" for the latter.
            Err(io::Error::other(format!(
                "config-lock background task did not complete: {join_err}"
            )))
        })
}

/// Write `contents` to `path` atomically: write to a `.tmp` sibling, fsync,
/// then rename over the destination. Call while holding this module's lock
/// (see the module doc comment) — this alone does not serialize concurrent
/// writers, it only prevents a torn/partial `config.toml` from a single
/// write.
///
/// `src/main.rs` and `bbs-web` each keep their own copy of this helper
/// (pre-dating this module); this one exists for `bbs-core` callers like
/// `BbsHost::persist_access_policy`, which had no atomic-write helper of
/// its own and was writing `config.toml` directly with `std::fs::write`.
pub(crate) fn atomic_write_file(path: &Path, contents: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let mut f = std::fs::File::create(&tmp)?;
    if let Err(e) = f.write_all(contents).and_then(|_| f.sync_all()) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    drop(f);
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    /// N concurrent writers, each holding the lock for a measurable amount
    /// of time around a shared counter, must never observe another writer
    /// "inside" their own critical section — proves the lock actually
    /// serializes, not just that it compiles and doesn't panic.
    #[test]
    fn concurrent_acquires_are_serialized() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();

        let concurrent_count = Arc::new(AtomicU32::new(0));
        let max_concurrent = Arc::new(AtomicU32::new(0));
        let mut handles = Vec::new();

        for _ in 0..16 {
            let config_path = config_path.clone();
            let concurrent_count = Arc::clone(&concurrent_count);
            let max_concurrent = Arc::clone(&max_concurrent);
            handles.push(std::thread::spawn(move || {
                with_config_lock_sync(&config_path, || {
                    let now = concurrent_count.fetch_add(1, Ordering::SeqCst) + 1;
                    max_concurrent.fetch_max(now, Ordering::SeqCst);
                    // Hold the lock briefly so overlapping acquires, if the
                    // lock didn't actually serialize, would have a real
                    // chance to be observed by another thread's
                    // fetch_add above.
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    concurrent_count.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "at most one thread should ever be inside the lock at once"
        );
    }

    #[test]
    fn lock_is_released_after_with_config_lock_sync_returns() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();

        with_config_lock_sync(&config_path, || Ok(())).unwrap();
        // A second acquire must not block/fail now that the first call's
        // guard has dropped.
        with_config_lock_sync(&config_path, || Ok(())).unwrap();
    }

    #[test]
    fn lock_is_released_even_when_f_errors() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();

        let err = with_config_lock_sync(&config_path, || {
            Err::<(), _>(io::Error::other("simulated failure"))
        });
        assert!(err.is_err());
        // Must not still be held after the erroring call returns.
        with_config_lock_sync(&config_path, || Ok(())).unwrap();
    }

    #[tokio::test]
    async fn with_config_lock_serializes_across_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();

        let concurrent_count = Arc::new(AtomicU32::new(0));
        let max_concurrent = Arc::new(AtomicU32::new(0));
        let mut tasks = Vec::new();

        for _ in 0..16 {
            let config_path = config_path.clone();
            let concurrent_count = Arc::clone(&concurrent_count);
            let max_concurrent = Arc::clone(&max_concurrent);
            tasks.push(tokio::spawn(async move {
                with_config_lock(&config_path, move || {
                    let now = concurrent_count.fetch_add(1, Ordering::SeqCst) + 1;
                    max_concurrent.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    concurrent_count.fetch_sub(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "at most one task should ever be inside the lock at once"
        );
    }

    /// The failure mode #227 is actually about: N concurrent read-modify-
    /// write cycles against the same file, each reading the current value,
    /// computing a new one, and writing it back — the classic lost-update
    /// race. Without a lock spanning the whole read+write, two overlapping
    /// increments can both read the same starting value and each write
    /// `starting + 1`, silently losing one of the two increments. With the
    /// lock, N concurrent increments must always produce exactly N in the
    /// end — this is a much stronger, more realistic proof than just
    /// showing the lock primitive itself provides mutual exclusion (the
    /// tests above): it exercises the actual read-modify-write pattern
    /// every real call site in src/main.rs and bbs-web uses.
    #[tokio::test]
    async fn with_config_lock_prevents_lost_updates_on_concurrent_increment() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "0").unwrap();

        const N: u32 = 50;
        let mut tasks = Vec::new();
        for _ in 0..N {
            let config_path = config_path.clone();
            let path_for_closure = config_path.clone();
            tasks.push(tokio::spawn(async move {
                with_config_lock(&config_path, move || {
                    let current: u32 = std::fs::read_to_string(&path_for_closure)?
                        .trim()
                        .parse()
                        .map_err(io::Error::other)?;
                    // A brief pause between read and write widens the race
                    // window a real read-parse-modify-write cycle has (TOML
                    // parsing + mutation + serialization all take real,
                    // nonzero time) — without the lock, this all but
                    // guarantees overlapping reads observe the same stale
                    // value.
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    std::fs::write(&path_for_closure, (current + 1).to_string())
                })
                .await
                .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let final_value: u32 = std::fs::read_to_string(&config_path)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(
            final_value, N,
            "every one of {N} concurrent increments must be preserved -- a \
             lower final value means the lock didn't actually serialize the \
             read-modify-write cycle and an update was lost"
        );
    }

    /// The CLI and the running server resolve `config_path` through
    /// independent logic and can spell the same file differently (a
    /// relative `config.toml` vs. the server's canonicalized absolute
    /// path). A symlinked alias is a stand-in for that: without
    /// canonicalizing before deriving the sidecar lock path, these two
    /// spellings would lock two different `.lock` files and provide zero
    /// mutual exclusion despite both operating on the same `config.toml`.
    #[test]
    fn lock_paths_that_canonicalize_to_the_same_file_contend() {
        let dir = tempfile::tempdir().unwrap();
        let real_path = dir.path().join("config.toml");
        std::fs::write(&real_path, "").unwrap();
        let alias_path = dir.path().join("config-alias.toml");
        std::os::unix::fs::symlink(&real_path, &alias_path).unwrap();

        let concurrent_count = Arc::new(AtomicU32::new(0));
        let max_concurrent = Arc::new(AtomicU32::new(0));

        let cc1 = Arc::clone(&concurrent_count);
        let mc1 = Arc::clone(&max_concurrent);
        let h1 = std::thread::spawn(move || {
            with_config_lock_sync(&real_path, || {
                let now = cc1.fetch_add(1, Ordering::SeqCst) + 1;
                mc1.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(50));
                cc1.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        });
        // Give h1 a head start so h2 reliably contends on the lock h1 is
        // already holding, rather than racing to acquire first.
        std::thread::sleep(std::time::Duration::from_millis(10));
        let cc2 = Arc::clone(&concurrent_count);
        let mc2 = Arc::clone(&max_concurrent);
        let h2 = std::thread::spawn(move || {
            with_config_lock_sync(&alias_path, || {
                let now = cc2.fetch_add(1, Ordering::SeqCst) + 1;
                mc2.fetch_max(now, Ordering::SeqCst);
                cc2.fetch_sub(1, Ordering::SeqCst);
                Ok(())
            })
            .unwrap();
        });
        h1.join().unwrap();
        h2.join().unwrap();

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "locking through a symlinked alias of the same file must still \
             serialize with the real path -- a lower canonicalization gap \
             would let both run concurrently"
        );
    }

    /// A local process with write access to the config directory could
    /// pre-plant `<config_path>.lock` as a symlink to an arbitrary file
    /// this process can write, hoping a locked config write truncates that
    /// file instead. `O_NOFOLLOW` must refuse to open through it.
    #[test]
    fn lock_sidecar_refuses_to_follow_a_preexisting_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let config_path = dir.path().join("config.toml");
        std::fs::write(&config_path, "").unwrap();

        let victim_path = dir.path().join("victim.txt");
        std::fs::write(&victim_path, "do not touch").unwrap();
        let lock_path = lock_file_path(&config_path);
        std::os::unix::fs::symlink(&victim_path, &lock_path).unwrap();

        let result = with_config_lock_sync(&config_path, || Ok(()));
        assert!(
            result.is_err(),
            "opening the lock sidecar through a pre-planted symlink must \
             fail, not silently follow it"
        );
        assert_eq!(
            std::fs::read_to_string(&victim_path).unwrap(),
            "do not touch",
            "the symlink target must be untouched -- O_NOFOLLOW should \
             reject the open before any write is attempted"
        );
    }
}
