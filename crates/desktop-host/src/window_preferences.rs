use std::{path::PathBuf, sync::Mutex};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ClosePreference {
    #[default]
    Tray,
    Minimize,
    Exit,
}

impl ClosePreference {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "tray" => Ok(Self::Tray),
            "minimize" => Ok(Self::Minimize),
            "exit" => Ok(Self::Exit),
            _ => Err("Window close preference must be tray, minimize, or exit".into()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tray => "tray",
            Self::Minimize => "minimize",
            Self::Exit => "exit",
        }
    }
}

pub struct WindowPreferences {
    path: PathBuf,
    close: Mutex<ClosePreference>,
}

impl WindowPreferences {
    pub fn load(config: PathBuf) -> Self {
        // Separate from installation preferences, whose writer replaces its whole file.
        let path = config.join("window-close-preference");
        let close = std::fs::read_to_string(&path)
            .ok()
            .and_then(|value| ClosePreference::parse(value.trim()).ok())
            .unwrap_or_default();
        Self {
            path,
            close: Mutex::new(close),
        }
    }

    pub fn get(&self) -> Result<ClosePreference, String> {
        self.close
            .lock()
            .map(|value| *value)
            .map_err(|_| "Window preferences unavailable".into())
    }

    pub fn set(&self, value: &str) -> Result<(), String> {
        let value = ClosePreference::parse(value)?;
        let mut current = self
            .close
            .lock()
            .map_err(|_| "Window preferences unavailable")?;
        let temporary = self.path.with_extension("tmp");
        std::fs::write(&temporary, value.as_str())
            .and_then(|()| std::fs::rename(&temporary, &self.path))
            .map_err(|_| "Cannot save window close preference")?;
        *current = value;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_supported_preferences() {
        for value in ["tray", "minimize", "exit"] {
            assert_eq!(ClosePreference::parse(value).unwrap().as_str(), value);
        }
        for value in ["", "kill", "EXIT", " minimize "] {
            assert!(ClosePreference::parse(value).is_err());
        }
    }

    #[test]
    fn persists_defaults_and_preserves_install_preferences() {
        let root =
            std::env::temp_dir().join(format!("desktop-window-preferences-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let installation = root.join("preferences.json");
        std::fs::write(&installation, "existing installation preferences").unwrap();
        let preferences = WindowPreferences::load(root.clone());
        assert_eq!(preferences.get().unwrap(), ClosePreference::Tray);
        for value in ["exit", "minimize", "tray"] {
            preferences.set(value).unwrap();
            assert_eq!(preferences.get().unwrap().as_str(), value);
            assert_eq!(
                WindowPreferences::load(root.clone())
                    .get()
                    .unwrap()
                    .as_str(),
                value
            );
        }
        assert!(preferences.set("kill").is_err());
        assert_eq!(preferences.get().unwrap(), ClosePreference::Tray);
        std::fs::write(&preferences.path, "invalid").unwrap();
        assert_eq!(
            WindowPreferences::load(root.clone()).get().unwrap(),
            ClosePreference::Tray
        );
        assert_eq!(
            std::fs::read_to_string(&installation).unwrap(),
            "existing installation preferences"
        );
        std::fs::remove_file(&preferences.path).unwrap();
        // A failed write must not change the effective preference.
        std::fs::create_dir(preferences.path.with_extension("tmp")).unwrap();
        assert!(preferences.set("exit").is_err());
        assert_eq!(preferences.get().unwrap(), ClosePreference::Tray);
        std::fs::remove_dir(preferences.path.with_extension("tmp")).unwrap();
        std::fs::remove_file(installation).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
}
