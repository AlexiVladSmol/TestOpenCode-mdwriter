//! Настройки приложения: загрузка, миграция и атомарная запись в JSON.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::state::ViewMode;
use crate::theme::ThemeChoice;

/// Настройки приложения (persist в JSON).
#[derive(Serialize, Deserialize)]
pub(crate) struct Settings {
    /// Легаси из старых настроек (`dark: true/false`) — только для миграции.
    #[serde(default, skip_serializing)]
    pub(crate) dark: Option<bool>,
    #[serde(default)]
    pub(crate) theme: ThemeChoice,
    #[serde(default)]
    pub(crate) view_mode: ViewMode,
    #[serde(default)]
    pub(crate) show_graph: bool,
    #[serde(default)]
    pub(crate) last_vault: Option<PathBuf>,
    #[serde(default)]
    pub(crate) window_size: Option<[f32; 2]>,
    #[serde(default = "default_tree_w")]
    pub(crate) tree_w: f32,
}

fn default_tree_w() -> f32 {
    240.0
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            dark: None,
            theme: ThemeChoice::System,
            view_mode: ViewMode::Split,
            show_graph: false,
            last_vault: None,
            window_size: None,
            tree_w: 240.0,
        }
    }
}

fn settings_path() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|p| p.join("mdwriter").join("settings.json"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")));
        base.map(|p| p.join("mdwriter").join("settings.json"))
    }
}

pub(crate) fn load_settings() -> Settings {
    let Some(path) = settings_path() else {
        return Settings::default();
    };
    let mut settings = match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str::<Settings>(&s).unwrap_or_else(|e| {
            eprintln!("settings parse failed ({e}); используем значения по умолчанию");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    };
    // Миграция старых настроек `dark: true/false`.
    if settings.theme == ThemeChoice::System {
        if let Some(dark) = settings.dark {
            settings.theme = if dark {
                ThemeChoice::Dark
            } else {
                ThemeChoice::Light
            };
        }
    }
    settings
}

/// Записывает настройки атомарно. Ошибка возвращается готовым текстом для UI.
pub(crate) fn save(settings: &Settings) -> Result<(), String> {
    let Some(path) = settings_path() else {
        return Ok(());
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    match serde_json::to_string_pretty(settings) {
        Ok(s) => {
            // Атомарная запись: оборванная запись не оставит битый JSON.
            vault::store::atomic_write(&path, s.as_bytes())
                .map_err(|e| format!("Не удалось сохранить настройки: {e}"))
        }
        Err(e) => Err(format!("Не удалось сериализовать настройки: {e}")),
    }
}

/// Нужно ли писать настройки в этом кадре: только при изменениях и не во время
/// перетаскивания (иначе разделитель панели пишет файл в каждом кадре).
pub(crate) fn should_save_settings(settings_dirty: bool, pointer_down: bool) -> bool {
    settings_dirty && !pointer_down
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_saved_only_when_dirty_and_not_dragging() {
        assert!(should_save_settings(true, false));
        // Пока зажата кнопка мыши (перетаскивание разделителя) не пишем.
        assert!(!should_save_settings(true, true));
        assert!(!should_save_settings(false, false));
        assert!(!should_save_settings(false, true));
    }
}
