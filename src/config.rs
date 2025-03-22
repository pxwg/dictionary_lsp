use crate::formatting::FormattingConfig;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
  pub formatting: FormattingConfig,
  pub dictionary_path: Option<String>,
  pub completion: CmpConfig,
  pub freq_path: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CmpConfig {
  pub enabled: bool,
  pub max_distance: u8,
}

// Config manager to hold shared in-memory configuration
pub struct ConfigManager {
  config: Arc<Mutex<Config>>,
}

// Singleton instance for global config access
static CONFIG_MANAGER: Lazy<Mutex<Config>> = Lazy::new(|| Mutex::new(Config::load_from_disk()));

impl ConfigManager {
  pub fn new() -> Self {
    let config = Config::load_from_disk();
    // debug output
    // eprintln!("Loaded config: {:#?}", config);
    Self {
      config: Arc::new(Mutex::new(config)),
    }
  }

  // Then get_config can be used as:
  pub fn get_config() -> Config {
    CONFIG_MANAGER.lock().unwrap().clone()
  }

  // Update the in-memory config
  pub fn update_config<F>(update_fn: F) -> Config
  where
    F: FnOnce(&mut Config),
  {
    let mut config = CONFIG_MANAGER.lock().unwrap();
    update_fn(&mut config);
    config.clone()
  }

  // Update config and save to disk
  pub fn update_and_save_config<F>(
    update_fn: F,
  ) -> Result<(Config, PathBuf), Box<dyn std::error::Error + Send + Sync>>
  where
    F: FnOnce(&mut Config),
  {
    let config = Self::update_config(update_fn);
    let path = Config::save_to_disk(&config)?;
    Ok((config, path))
  }
}

impl Default for Config {
  fn default() -> Self {
    Self {
      formatting: FormattingConfig::default(),
      dictionary_path: None,
      freq_path: None,
      completion: CmpConfig {
        max_distance: 3,
        enabled: true,
      },
    }
  }
}

impl Config {
  // Get the current global configuration
  pub fn get() -> Self {
    CONFIG_MANAGER.lock().unwrap().clone()
  }

  // Update the configuration in memory only (no write to disk)
  pub fn update<F>(update_fn: F)
  where
    F: FnOnce(&mut Config),
  {
    let mut config = CONFIG_MANAGER.lock().unwrap();
    update_fn(&mut config);
  }

  pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
    let contents = fs::read_to_string(path)?;
    let config: Config = toml::from_str(&contents)?;
    Ok(config)
  }

  pub fn is_sqlite(path: Option<&str>) -> bool {
    match path {
      Some(path) => path.ends_with(".db"),
      None => false,
    }
  }

  // Load config from disk
  pub fn load_from_disk() -> Self {
    let possible_paths =
      vec![dirs::home_dir().map(|p| p.join(".config/dictionary-lsp/config.toml"))];

    for path in possible_paths.into_iter().flatten() {
      if let Ok(config) = Self::load_from_file(&path) {
        // debug output
        // eprintln!("Loaded config from: {}", path.display());
        return config;
      }
    }

    Self::default()
  }

  // Save config to disk
  pub fn save_to_disk(
    config: &Config,
  ) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let possible_paths =
      vec![dirs::home_dir().map(|p| p.join(".config/dictionary-lsp/config.toml"))];

    let path = possible_paths
      .into_iter()
      .flatten()
      .next()
      .ok_or_else(|| "No valid path found to save config".to_string())?;

    if let Some(parent) = path.parent() {
      fs::create_dir_all(parent)?;
    }

    let toml = toml::to_string(config)?;
    fs::write(&path, toml)?;
    Ok(path)
  }
}
