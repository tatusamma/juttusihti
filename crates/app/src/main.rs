//! Juttusihti desktop application.

mod app;
mod db;
mod format;
mod graph_view;
mod labels;
mod map_view;
mod providers;
mod reports;
mod research;
mod review;
mod secrets;
mod settings;
mod timeline_view;

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
    // The renderer is chosen before the window exists. An explicit
    // ICED_BACKEND from the environment wins over the setting.
    if std::env::var_os("ICED_BACKEND").is_none() && settings::read(&store).is_ok_and(|v| v.cpu_renderer) {
        // SAFETY: still single-threaded here; nothing else reads or writes
        // the environment concurrently.
        unsafe { std::env::set_var("ICED_BACKEND", "tiny-skia") };
    }
    let db = db::Db::new(store);

    iced::application(move || app::App::boot(db.clone(), db_path.clone()), app::App::update, app::App::view)
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(iced::window::Settings {
            size: iced::Size::new(1280.0, 800.0),
            min_size: Some(iced::Size::new(900.0, 560.0)),
            icon: window_icon(),
            ..Default::default()
        })
        .run()
}

fn window_icon() -> Option<iced::window::Icon> {
    let png = include_bytes!("../../../packaging/icons/juttusihti-64.png");
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.into_rgba8();
    let (w, h) = img.dimensions();
    iced::window::icon::from_rgba(img.into_raw(), w, h).ok()
}
