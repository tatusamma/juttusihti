//! Keeps panic messages in a file next to the database. Release builds have
//! no console on Windows, so without this the cause of a panic is lost.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// The log starts over once it has grown past this.
const MAX_BYTES: u64 = 1024 * 1024;

pub fn install(path: PathBuf) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let version = env!("CARGO_PKG_VERSION");
        let backtrace = std::backtrace::Backtrace::force_capture();
        append(&path, &format!("{} v{version} {info}\n{backtrace}\n", chrono::Utc::now().to_rfc3339()));
        previous(info);
    }));
}

/// Best effort: a panic is already being handled, so failing to log it is ignored.
fn append(path: &Path, entry: &str) {
    if fs::metadata(path).is_ok_and(|m| m.len() > MAX_BYTES) {
        let _ = fs::remove_file(path);
    }
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(entry.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_appended_until_the_log_is_too_large() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("crash.log");
        append(&path, "first\n");
        append(&path, "second\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\n");

        fs::write(&path, vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        append(&path, "third\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "third\n");
    }
}
