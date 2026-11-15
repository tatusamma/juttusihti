//! Errors that stop Juttusihti before its window opens. A Windows release build has no console,
//! so they are shown in a message box as well as written to stderr and the log.

use std::path::Path;

use juttusihti_core::Error;

/// Shows `message` and exits. Never returns.
pub fn fail(message: &str) -> ! {
    eprintln!("{message}");
    tracing::error!(message, "Juttusihti could not start");
    rfd::MessageDialog::new()
        .set_level(rfd::MessageLevel::Error)
        .set_title("Juttusihti could not start")
        .set_description(message)
        .set_buttons(rfd::MessageButtons::Ok)
        .show();
    std::process::exit(1);
}

/// What to say when the data folder cannot be created.
pub fn data_dir_text(dir: &Path, e: &std::io::Error) -> String {
    format!(
        "Juttusihti cannot create its data folder\n{}\n\n{e}\n\nCheck that the folder can be written to, \
         or set JUTTUSIHTI_DATA_DIR to another folder.",
        dir.display()
    )
}

/// What to say when the database cannot be opened.
pub fn open_text(path: &Path, e: &Error) -> String {
    let file = path.display();
    match e {
        Error::SchemaTooNew { .. } => format!(
            "The database\n{file}\n\nwas updated by a newer version of Juttusihti, and this version cannot open \
             it. Use the newer version, or quit and replace the database with the copy saved next to it before \
             the update (juttusihti-before-schema-N.db) to go back to the data as it was then.\n\n({e})"
        ),
        Error::Backup(why) => format!(
            "Juttusihti needs to update the database\n{file}\n\nbut first saves a copy of it next to it, and that \
             failed, so nothing was changed:\n{why}\n\nFree some disk space or check that the folder can be \
             written to, then start Juttusihti again."
        ),
        _ => format!("Juttusihti cannot open the database\n{file}\n\n{e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_database_says_how_to_go_back() {
        let t = open_text(Path::new("C:/data/juttusihti.db"), &Error::SchemaTooNew { found: 12, supported: 11 });
        assert!(t.contains("C:/data/juttusihti.db") && t.contains("newer version"));
        assert!(t.contains("juttusihti-before-schema-N.db"));
    }

    #[test]
    fn a_failed_backup_says_nothing_was_changed() {
        let t = open_text(Path::new("j.db"), &Error::Backup("disk full".into()));
        assert!(t.contains("nothing was changed") && t.contains("disk full"));
    }

    #[test]
    fn other_errors_show_the_path_and_the_reason() {
        let t = open_text(Path::new("j.db"), &Error::Invalid("broken".into()));
        assert!(t.contains("j.db") && t.contains("broken"));
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let t = data_dir_text(Path::new("C:/data"), &io);
        assert!(t.contains("C:/data") && t.contains("access denied") && t.contains("JUTTUSIHTI_DATA_DIR"));
    }
}
