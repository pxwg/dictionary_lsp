use crate::formatting::FormattingConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub formatting: FormattingConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            formatting: FormattingConfig::default(),
        }
    }
}

impl Config {
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let contents = fs::read_to_string(path)?;
        let config: Config = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Gets config from standard locations or creates default if not found
    pub fn get() -> Self {
        let possible_paths = vec![
            Some(PathBuf::from("./dictionary-lsp.toml")),
            dirs::config_dir().map(|p| p.join("dictionary-lsp/config.toml")),
            dirs::home_dir().map(|p| p.join(".config/dictionary-lsp/config.toml")),
        ];

        for path in possible_paths.into_iter().flatten() {
            if let Ok(config) = Self::load_from_file(&path) {
                return config;
            }
        }

        Self::default()
    }
}
