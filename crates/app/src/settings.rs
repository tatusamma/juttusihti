//! Settings screen: retention, refresh interval and appearance, stored in
//! the SQLite `settings` table.

use iced::widget::{checkbox, column, row, scrollable, text, text_input};
use iced::{Center, Element, Fill, Task as Cmd};

use crate::db::Db;
use crate::style::{self, SIZE_BODY, SIZE_SMALL, SPACE};

pub const KEY_RETENTION_DAYS: &str = "retention_days";
pub const KEY_REFRESH_MINUTES: &str = "refresh_minutes";
pub const KEY_THEME: &str = "theme";
pub const KEY_RENDERER: &str = "renderer";

pub const DEFAULT_REFRESH_MINUTES: u32 = 30;

/// Values the rest of the app uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Values {
    /// `None` keeps articles forever.
    pub retention_days: Option<u32>,
    /// 0 disables automatic refresh.
    pub refresh_minutes: u32,
    pub dark: bool,
    /// Software rendering: far less memory, slower drawing. Applied at startup.
    pub cpu_renderer: bool,
}

impl Default for Values {
    fn default() -> Self {
        Self { retention_days: None, refresh_minutes: DEFAULT_REFRESH_MINUTES, dark: true, cpu_renderer: false }
    }
}

/// Reads settings, falling back to defaults for missing or invalid values.
pub fn read(store: &juttusihti_core::store::Store) -> juttusihti_core::Result<Values> {
    let d = Values::default();
    Ok(Values {
        retention_days: store.setting(KEY_RETENTION_DAYS)?.and_then(|v| v.parse().ok()).filter(|d| *d > 0),
        refresh_minutes: store.setting(KEY_REFRESH_MINUTES)?.and_then(|v| v.parse().ok()).unwrap_or(d.refresh_minutes),
        dark: store.setting(KEY_THEME)?.is_none_or(|v| v != "light"),
        cpu_renderer: store.setting(KEY_RENDERER)?.is_some_and(|v| v == "cpu"),
    })
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Result<Values, String>),
    Retention(String),
    Refresh(String),
    /// Handled by the app: switch to the dark (true) or light theme now.
    SetDark(bool),
    /// Handled by the app: the same OPML import and export as the feeds dropdown.
    ImportOpml,
    ExportOpml,
    CpuRenderer(bool),
    Save,
    Saved(Result<(Values, usize), String>),
    ThemeSaved(Result<(), String>),
    MapLoaded(Option<String>, u64),
    TileUrl(String),
    SaveTileUrl,
    ClearTiles,
    MapDone(Result<String, String>),
}

pub struct State {
    db: Db,
    db_path: std::path::PathBuf,
    pub values: Values,
    retention: String,
    refresh: String,
    dark: bool,
    cpu_renderer: bool,
    tile_url: String,
    tile_cache_bytes: u64,
    message: Option<Result<String, String>>,
}

impl State {
    pub fn new(db: Db, db_path: std::path::PathBuf) -> Self {
        let v = Values::default();
        Self {
            db,
            db_path,
            values: v,
            retention: String::new(),
            refresh: v.refresh_minutes.to_string(),
            dark: v.dark,
            cpu_renderer: v.cpu_renderer,
            tile_url: juttusihti_fetch::tiles::OSM_TILE_URL.into(),
            tile_cache_bytes: 0,
            message: None,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        let values = Cmd::perform(async move { db.call(|s| read(s)).await }, Msg::Loaded);
        let (db, tiles) = (self.db.clone(), self.tile_cache());
        let map = Cmd::perform(
            async move {
                let url = db.call(|s| s.setting(crate::map_view::KEY_TILE_URL)).await.ok().flatten();
                let size = tokio::task::spawn_blocking(move || tiles.size()).await.unwrap_or(0);
                (url, size)
            },
            |(url, size)| Msg::MapLoaded(url, size),
        );
        Cmd::batch([values, map])
    }

    fn tile_cache(&self) -> juttusihti_fetch::tiles::TileCache {
        let dir = self.db_path.parent().map(|p| p.join("tiles")).unwrap_or_else(|| "tiles".into());
        juttusihti_fetch::tiles::TileCache::new(dir)
    }

    fn sync_form(&mut self) {
        self.retention = self.values.retention_days.map(|d| d.to_string()).unwrap_or_default();
        self.refresh = self.values.refresh_minutes.to_string();
        self.dark = self.values.dark;
        self.cpu_renderer = self.values.cpu_renderer;
    }

    fn parse(&self) -> Result<Values, String> {
        let retention_days = match self.retention.trim() {
            "" | "0" => None,
            t => Some(t.parse::<u32>().map_err(|_| "Retention must be a whole number of days.")?),
        };
        if retention_days.is_some_and(|d| d < 7) {
            return Err("Retention shorter than 7 days is not allowed, to avoid accidental data loss.".into());
        }
        let refresh_minutes: u32 =
            self.refresh.trim().parse().map_err(|_| "Refresh interval must be a whole number of minutes.")?;
        if refresh_minutes != 0 && refresh_minutes < 5 {
            return Err("Refresh at most every 5 minutes, to be polite to feed servers.".into());
        }
        Ok(Values { retention_days, refresh_minutes, dark: self.dark, cpu_renderer: self.cpu_renderer })
    }

    /// Persists only the theme; used by the toolbar toggle.
    pub fn save_theme(&mut self, dark: bool) -> Cmd<Msg> {
        self.values.dark = dark;
        self.dark = dark;
        let db = self.db.clone();
        Cmd::perform(
            async move { db.call(move |s| s.set_setting(KEY_THEME, if dark { "dark" } else { "light" })).await },
            Msg::ThemeSaved,
        )
    }

    pub fn update(&mut self, msg: Msg) -> Cmd<Msg> {
        match msg {
            Msg::Loaded(Ok(v)) => {
                self.values = v;
                self.sync_form();
            }
            Msg::Loaded(Err(e)) => self.message = Some(Err(e)),
            Msg::Retention(s) => self.retention = s,
            Msg::Refresh(s) => self.refresh = s,
            Msg::SetDark(_) | Msg::ImportOpml | Msg::ExportOpml => {}
            Msg::CpuRenderer(on) => self.cpu_renderer = on,
            Msg::Save => match self.parse() {
                Err(e) => self.message = Some(Err(e)),
                Ok(v) => {
                    let db = self.db.clone();
                    return Cmd::perform(
                        async move {
                            db.call(move |s| -> juttusihti_core::Result<(Values, usize)> {
                                s.set_setting(
                                    KEY_RETENTION_DAYS,
                                    &v.retention_days.map(|d| d.to_string()).unwrap_or_else(|| "0".into()),
                                )?;
                                s.set_setting(KEY_REFRESH_MINUTES, &v.refresh_minutes.to_string())?;
                                s.set_setting(KEY_THEME, if v.dark { "dark" } else { "light" })?;
                                s.set_setting(KEY_RENDERER, if v.cpu_renderer { "cpu" } else { "gpu" })?;
                                let removed = match v.retention_days {
                                    Some(d) => s.apply_retention(d)?,
                                    None => 0,
                                };
                                Ok((v, removed))
                            })
                            .await
                        },
                        Msg::Saved,
                    );
                }
            },
            Msg::ThemeSaved(Ok(())) => {}
            Msg::ThemeSaved(Err(e)) => self.message = Some(Err(e)),
            Msg::Saved(Ok((v, removed))) => {
                self.values = v;
                self.sync_form();
                self.message = Some(Ok(if removed > 0 {
                    format!("Saved. Removed {removed} articles older than the retention period.")
                } else {
                    "Saved.".into()
                }));
            }
            Msg::Saved(Err(e)) => self.message = Some(Err(e)),
            Msg::MapLoaded(url, size) => {
                if let Some(u) = url {
                    self.tile_url = u;
                }
                self.tile_cache_bytes = size;
            }
            Msg::TileUrl(u) => self.tile_url = u,
            Msg::SaveTileUrl => {
                let url = self.tile_url.trim().to_string();
                if let Err(e) = juttusihti_fetch::tiles::validate_template(&url) {
                    self.message = Some(Err(e.to_string()));
                    return Cmd::none();
                }
                let db = self.db.clone();
                return Cmd::perform(
                    async move {
                        db.call(move |s| s.set_setting(crate::map_view::KEY_TILE_URL, &url)).await?;
                        Ok("Saved the tile server. Reopen the map to use it.".to_string())
                    },
                    Msg::MapDone,
                );
            }
            Msg::ClearTiles => {
                let tiles = self.tile_cache();
                return Cmd::perform(
                    async move {
                        tokio::task::spawn_blocking(move || tiles.clear().map_err(|e| e.to_string()))
                            .await
                            .map_err(|e| e.to_string())??;
                        Ok("Cleared the map tile cache.".to_string())
                    },
                    Msg::MapDone,
                );
            }
            Msg::MapDone(r) => {
                self.message = Some(r);
                return self.load();
            }
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        fn field<'a>(placeholder: &'a str, value: &'a str, on: fn(String) -> Msg, width: f32) -> Element<'a, Msg> {
            text_input(placeholder, value).on_input(on).size(SIZE_BODY).padding(SPACE[1]).width(width).into()
        }
        let label = |s: &'static str| text(s).size(SIZE_BODY);
        let storage = column![
            style::heading("Storage"),
            row![
                label("Keep articles for"),
                field("forever", &self.retention, Msg::Retention, 90.0),
                label("days (empty = forever)"),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
            style::note(
                "Saved articles, annotated articles, confirmed research links and articles in analysis sessions are always kept."
            ),
        ]
        .spacing(SPACE[1]);
        let feeds = column![
            style::heading("Feeds"),
            row![
                label("Refresh feeds every"),
                field("30", &self.refresh, Msg::Refresh, 70.0),
                label("minutes (0 = manual only)"),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
            row![
                style::quiet_button(None, "Import OPML", style::quiet, Some(Msg::ImportOpml)),
                style::quiet_button(None, "Export OPML", style::quiet, Some(Msg::ExportOpml)),
            ]
            .spacing(SPACE[1]),
        ]
        .spacing(SPACE[1]);
        let appearance = column![
            style::heading("Appearance"),
            row![
                style::segmented([(true, "Dark"), (false, "Light")], self.values.dark, Msg::SetDark),
                checkbox(self.cpu_renderer)
                    .label(
                        "CPU renderer: uses far less memory, draws maps and graphs more slowly (applies after restart)"
                    )
                    .size(SIZE_BODY)
                    .text_size(SIZE_BODY)
                    .style(style::checkbox)
                    .on_toggle(Msg::CpuRenderer),
            ]
            .spacing(SPACE[3])
            .align_y(Center),
        ]
        .spacing(SPACE[1]);
        let map = column![
            style::heading("Map"),
            row![
                text_input(juttusihti_fetch::tiles::OSM_TILE_URL, &self.tile_url)
                    .on_input(Msg::TileUrl)
                    .on_submit(Msg::SaveTileUrl)
                    .size(SIZE_BODY)
                    .padding(SPACE[1])
                    .width(Fill),
                style::quiet_button(None, "Save", style::quiet, Some(Msg::SaveTileUrl)),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
            style::note("Tile server URL (https, with {z}/{x}/{y}). Respect the provider's usage policy."),
            row![
                style::note(format!("Tile cache: {:.1} MB", self.tile_cache_bytes as f64 / 1_048_576.0)),
                style::quiet_button(None, "Clear map tile cache", style::quiet, Some(Msg::ClearTiles)),
            ]
            .spacing(SPACE[1])
            .align_y(Center),
        ]
        .spacing(SPACE[1]);
        let mut col = column![
            style::title("Settings"),
            storage,
            feeds,
            appearance,
            style::main_button("Save", Some(Msg::Save)),
            map,
            text(format!("Database: {}", self.db_path.display())).size(SIZE_SMALL).style(style::faint),
        ]
        .spacing(SPACE[4])
        .padding(SPACE[4])
        .max_width(720);
        if let Some(m) = &self.message {
            col = col.push(style::message(m));
        }
        scrollable(col).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_defaults_and_stored_values() {
        let s = juttusihti_core::store::Store::open_in_memory().unwrap();
        assert_eq!(read(&s).unwrap(), Values::default());
        s.set_setting(KEY_RETENTION_DAYS, "90").unwrap();
        s.set_setting(KEY_REFRESH_MINUTES, "0").unwrap();
        s.set_setting(KEY_THEME, "light").unwrap();
        assert_eq!(
            read(&s).unwrap(),
            Values { retention_days: Some(90), refresh_minutes: 0, dark: false, cpu_renderer: false }
        );
        s.set_setting(KEY_RETENTION_DAYS, "0").unwrap();
        s.set_setting(KEY_REFRESH_MINUTES, "junk").unwrap();
        let v = read(&s).unwrap();
        assert_eq!((v.retention_days, v.refresh_minutes), (None, DEFAULT_REFRESH_MINUTES));
    }

    #[test]
    fn validates_form() {
        let db = Db::new(juttusihti_core::store::Store::open_in_memory().unwrap());
        let mut st = State::new(db, std::path::PathBuf::from("test.db"));
        st.retention = "3".into();
        assert!(st.parse().is_err());
        st.retention = "30".into();
        st.refresh = "1".into();
        assert!(st.parse().is_err());
        st.refresh = "0".into();
        assert_eq!(st.parse().unwrap().retention_days, Some(30));
    }
}
