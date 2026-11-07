//! The throwaway browser profile directory.
//!
//! Removing it is a privacy guarantee, not a best effort: the directory is created exclusively
//! (so the helper never deletes anything it did not create), removed with retries (Windows keeps
//! a file locked briefly after the thread that wrote it has exited), and removed by a drop guard
//! too, so an early return or a panic cannot leave it behind. Failures are reported on stderr;
//! stdout only ever carries the outcome line.

use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

/// Waits between removal attempts; the last one is the total extra time a stuck file may get.
const REMOVE_BACKOFF: &[Duration] = &[
    Duration::from_millis(50),
    Duration::from_millis(100),
    Duration::from_millis(200),
    Duration::from_millis(400),
    Duration::from_millis(800),
    Duration::from_millis(1600),
];

pub struct Profile {
    path: Option<PathBuf>,
}

impl Profile {
    /// Creates `path`, which must not exist yet. Its parent must exist.
    pub fn create(path: &Path) -> io::Result<Profile> {
        std::fs::create_dir(path)?;
        Ok(Profile { path: Some(path.to_path_buf()) })
    }

    pub fn path(&self) -> &Path {
        self.path.as_deref().expect("profile not removed yet")
    }

    /// Removes the directory now (the drop guard does the same if this is never called).
    pub fn remove(mut self) {
        if let Some(path) = self.path.take() {
            remove_with_retries(&path);
        }
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            remove_with_retries(&path);
        }
    }
}

fn remove_with_retries(path: &Path) {
    let mut last_error = None;
    for delay in std::iter::once(&Duration::ZERO).chain(REMOVE_BACKOFF) {
        thread::sleep(*delay);
        match std::fs::remove_dir_all(path) {
            Ok(()) => return,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return,
            Err(e) => last_error = Some(e),
        }
    }
    if let Some(e) = last_error {
        eprintln!("juttusihti-browser: could not remove the profile directory {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn existing_directory_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Profile::create(dir.path()).is_err());
        assert!(dir.path().exists());
    }

    #[test]
    fn guard_removes_directory_with_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile");
        {
            let profile = Profile::create(&path).unwrap();
            std::fs::create_dir_all(profile.path().join("webstorage/x")).unwrap();
            std::fs::write(profile.path().join("webstorage/x/data"), b"state").unwrap();
        }
        assert!(!path.exists());
    }

    #[test]
    fn guard_removes_directory_on_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profile");
        let inner = path.clone();
        let result = std::panic::catch_unwind(move || {
            let _profile = Profile::create(&inner).unwrap();
            panic!("boom");
        });
        assert!(result.is_err());
        assert!(!path.exists());
    }
}
