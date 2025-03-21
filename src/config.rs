use crate::formatting::FormattingConfig;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
  pub formatting: FormattingConfig,
  pub dictionary_path: Option<String>,
  pub completion: CmpConfig,
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
lazy_static::lazy_static! {
  static ref CONFIG_MANAGER: ConfigManager = ConfigManager::new();
}

impl ConfigManager {
  fn new() -> Self {
    let config = Config::load_from_disk();
    // debug output
    // eprintln!("Loaded config: {:#?}", config);
    Self {
      config: Arc::new(Mutex::new(config)),
    }
  }

  // Get a clone of the current config
  pub fn get_config() -> Config {
    CONFIG_MANAGER.config.lock().unwrap().clone()
  }

  // Update the in-memory config
  pub fn update_config<F>(update_fn: F) -> Config
  where
    F: FnOnce(&mut Config),
  {
    let mut config = CONFIG_MANAGER.config.lock().unwrap();
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
      completion: CmpConfig {
        max_distance: 3,
        enabled: true,
      },
    }
  }
}

impl Config {
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
    let possible_paths = vec![
      dirs::config_dir().map(|p| p.join("dictionary-lsp/config.toml")),
      dirs::home_dir().map(|p| p.join(".config/dictionary-lsp/config.toml")),
      Some(PathBuf::from("./dictionary-lsp.toml")),
    ];

    for path in possible_paths.into_iter().flatten() {
      if let Ok(config) = Self::load_from_file(&path) {
        eprintln!("Loaded config from: {}", path.display());
        return config;
      }
    }

    Self::default()
  }

  // Save config to disk
  pub fn save_to_disk(
    config: &Config,
  ) -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let possible_paths = vec![
      dirs::config_dir().map(|p| p.join("dictionary-lsp/config.toml")),
      dirs::home_dir().map(|p| p.join(".config/dictionary-lsp/config.toml")),
      Some(PathBuf::from("./dictionary-lsp.toml")),
    ];

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

  // Compatibility function for existing code
  pub fn get() -> Self {
    ConfigManager::get_config()
  }
}
