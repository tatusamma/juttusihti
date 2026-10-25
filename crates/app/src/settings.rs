//! Settings screen: retention, refresh interval and appearance, stored in
//! the SQLite `settings` table.

use iced::widget::{button, checkbox, column, container, row, scrollable, text, text_input};
use iced::{Element, Font, Task as Cmd, font};

use crate::db::Db;

pub const KEY_RETENTION_DAYS: &str = "retention_days";
pub const KEY_REFRESH_MINUTES: &str = "refresh_minutes";
pub const KEY_THEME: &str = "theme";

pub const DEFAULT_REFRESH_MINUTES: u32 = 30;

/// Values the rest of the app uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Values {
    /// `None` keeps articles forever.
    pub retention_days: Option<u32>,
    /// 0 disables automatic refresh.
    pub refresh_minutes: u32,
    pub dark: bool,
}

impl Default for Values {
    fn default() -> Self {
        Self { retention_days: None, refresh_minutes: DEFAULT_REFRESH_MINUTES, dark: true }
    }
}

/// Reads settings, falling back to defaults for missing or invalid values.
pub fn read(store: &juttusihti_core::store::Store) -> juttusihti_core::Result<Values> {
    let d = Values::default();
    Ok(Values {
        retention_days: store.setting(KEY_RETENTION_DAYS)?.and_then(|v| v.parse().ok()).filter(|d| *d > 0),
        refresh_minutes: store.setting(KEY_REFRESH_MINUTES)?.and_then(|v| v.parse().ok()).unwrap_or(d.refresh_minutes),
        dark: store.setting(KEY_THEME)?.is_none_or(|v| v != "light"),
    })
}

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Result<Values, String>),
    Retention(String),
    Refresh(String),
    Dark(bool),
    Save,
    Saved(Result<(Values, usize), String>),
    ThemeSaved(Result<(), String>),
}

pub struct State {
    db: Db,
    db_path: std::path::PathBuf,
    pub values: Values,
    retention: String,
    refresh: String,
    dark: bool,
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
            message: None,
        }
    }

    pub fn load(&self) -> Cmd<Msg> {
        let db = self.db.clone();
        Cmd::perform(async move { db.call(|s| read(s)).await }, Msg::Loaded)
    }

    fn sync_form(&mut self) {
        self.retention = self.values.retention_days.map(|d| d.to_string()).unwrap_or_default();
        self.refresh = self.values.refresh_minutes.to_string();
        self.dark = self.values.dark;
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
        Ok(Values { retention_days, refresh_minutes, dark: self.dark })
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
            Msg::Dark(d) => self.dark = d,
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
        }
        Cmd::none()
    }

    pub fn view(&self) -> Element<'_, Msg> {
        let bold = Font { weight: font::Weight::Bold, ..Font::DEFAULT };
        let mut col = column![
            text("Settings").size(24).font(bold),
            text("Storage").size(16).font(bold),
            row![
                text("Keep articles for").size(14),
                text_input("forever", &self.retention).on_input(Msg::Retention).width(90),
                text("days (empty = forever)").size(14),
            ]
            .spacing(8)
            .align_y(iced::Center),
            text("Saved articles, annotated articles, confirmed research links and articles in analysis sessions are always kept.")
                .size(12),
            text("Feeds").size(16).font(bold),
            row![
                text("Refresh feeds every").size(14),
                text_input("30", &self.refresh).on_input(Msg::Refresh).width(70),
                text("minutes (0 = manual only)").size(14),
            ]
            .spacing(8)
            .align_y(iced::Center),
            text("Appearance").size(16).font(bold),
            checkbox(self.dark).label("Dark theme").on_toggle(Msg::Dark),
            button(text("Save")).on_press(Msg::Save),
            text(format!("Database: {}", self.db_path.display())).size(11),
        ]
        .spacing(14)
        .padding(24)
        .max_width(720);
        match &self.message {
            Some(Ok(m)) => col = col.push(container(text(m).size(13)).padding(8).style(container::success)),
            Some(Err(m)) => col = col.push(container(text(m).size(13)).padding(8).style(container::danger)),
            None => {}
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
        assert_eq!(read(&s).unwrap(), Values { retention_days: Some(90), refresh_minutes: 0, dark: false });
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
