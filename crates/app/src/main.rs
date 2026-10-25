//! Juttusihti desktop application.

mod app;
mod db;
mod format;
mod labels;
mod providers;
mod reports;
mod research;
mod review;
mod secrets;
mod settings;

use std::path::PathBuf;

fn data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("JUTTUSIHTI_DATA_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("fi", "Juttusihti", "Juttusihti")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

fn main() -> iced::Result {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("JUTTUSIHTI_LOG")
                .unwrap_or_else(|_| "warn,juttusihti=info".into()),
        )
        .init();

    let dir = data_dir();
    if let Err(e) = std::fs::create_dir_all(&dir) {
        eprintln!("cannot create data directory {}: {e}", dir.display());
        std::process::exit(1);
    }
    let db_path = dir.join("juttusihti.db");
    let store = match juttusihti_core::store::Store::open(&db_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cannot open database {}: {e}", db_path.display());
            std::process::exit(1);
        }
    };
    tracing::info!(path = %db_path.display(), "database opened");
    let db = db::Db::new(store);

    iced::application(move || app::App::boot(db.clone(), db_path.clone()), app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window_size((1280.0, 800.0))
        .run()
}
