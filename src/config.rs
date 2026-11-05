//! `~/.config/collage/config.toml` — every key is optional.

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// How many clipboard entries to keep (notes are never trimmed).
    pub history_limit: usize,
    /// How long the toast stays fully visible, in milliseconds.
    pub toast_duration_ms: u64,
    /// Distance between the toast and the bottom of the screen (logical px).
    pub toast_bottom_margin: i32,
    /// Toast font size (logical px).
    pub toast_font_size: f32,
    /// Show a preview of the copied text in the toast.
    pub toast_preview: bool,
    /// Keep the clipboard alive after the app you copied from closes.
    pub keep_clipboard: bool,
    /// Ignore clipboard contents bigger than this (bytes).
    pub max_entry_bytes: usize,
    /// Show a system tray icon.
    pub tray: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            history_limit: 500,
            toast_duration_ms: 1400,
            toast_bottom_margin: 56,
            toast_font_size: 15.0,
            toast_preview: true,
            keep_clipboard: true,
            max_entry_bytes: 4 * 1024 * 1024,
            tray: true,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("collage")
        .join("config.toml")
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str(&text) {
                Ok(cfg) => cfg,
                Err(err) => {
                    log::warn!("{}: {err} — using defaults", path.display());
                    Self::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => {
                log::warn!("{}: {err} — using defaults", path.display());
                Self::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_matches_defaults() {
        let cfg: Config = toml::from_str(include_str!("../config.example.toml")).unwrap();
        let def = Config::default();
        assert_eq!(cfg.history_limit, def.history_limit);
        assert_eq!(cfg.toast_duration_ms, def.toast_duration_ms);
        assert_eq!(cfg.max_entry_bytes, def.max_entry_bytes);
        assert_eq!(cfg.keep_clipboard, def.keep_clipboard);
    }

    #[test]
    fn partial_config_fills_defaults() {
        let cfg: Config = toml::from_str("history_limit = 7").unwrap();
        assert_eq!(cfg.history_limit, 7);
        assert!(cfg.tray);
        assert!(toml::from_str::<Config>("bogus = 1").is_err());
    }
}
