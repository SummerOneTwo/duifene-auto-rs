use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

const ENV_CONFIG_PATH: &str = "DUIFENE_CONFIG_PATH";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CourseLocation {
    #[serde(default)]
    pub longitude: String,
    #[serde(default)]
    pub latitude: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub cookie: String,
    #[serde(default)]
    pub locations: BTreeMap<String, CourseLocation>,
    #[serde(default)]
    pub learned_center: Option<(f64, f64)>,
    /// 签到门槛:已签到人数占比达到该百分比(0-100)才自动签到;0 表示不启用该限制
    #[serde(default = "default_signed_percent")]
    pub signed_percent: u32,
}

fn default_signed_percent() -> u32 {
    30
}

/// 保存签到百分比门槛(设置页)
pub fn save_signed_percent(percent: u32) -> io::Result<()> {
    let mut config = load_config();
    config.signed_percent = percent.clamp(0, 100);
    write_config(&config)
}

fn config_path() -> PathBuf {
    if let Ok(path) = env::var(ENV_CONFIG_PATH) {
        return PathBuf::from(path);
    }
    dirs::config_dir()
        .map(|directory| directory.join("duifene").join("config.json"))
        .unwrap_or_else(|| PathBuf::from("duifene-config.json"))
}

fn write_config(config: &AppConfig) -> io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config).map_err(io::Error::other)?;
    fs::write(path, text)
}

pub fn load_config() -> AppConfig {
    fs::read_to_string(config_path())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn load_learned_center() -> Option<(f64, f64)> {
    load_config().learned_center
}

pub fn save_learned_center(center: (f64, f64)) -> io::Result<()> {
    let mut config = load_config();
    config.learned_center = Some(center);
    write_config(&config)
}

pub fn save_cookie(cookie_string: &str) -> io::Result<()> {
    let mut config = load_config();
    config.cookie = cookie_string.to_string();
    write_config(&config)
}

pub fn save_course_location(name: &str, longitude: &str, latitude: &str) -> io::Result<()> {
    let mut config = load_config();
    config.locations.insert(
        name.to_string(),
        CourseLocation {
            longitude: longitude.to_string(),
            latitude: latitude.to_string(),
        },
    );
    write_config(&config)
}

pub fn course_coordinates(config: &AppConfig) -> HashMap<String, (f64, f64)> {
    let mut result = HashMap::new();
    for (name, location) in &config.locations {
        if let (Ok(longitude), Ok(latitude)) = (
            location.longitude.parse::<f64>(),
            location.latitude.parse::<f64>(),
        ) {
            result.insert(name.clone(), (longitude, latitude));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip() {
        let mut config = AppConfig {
            cookie: "session=abc; token=xyz".to_string(),
            ..Default::default()
        };
        config.locations.insert(
            "test".to_string(),
            CourseLocation {
                longitude: "114.10".to_string(),
                latitude: "22.70".to_string(),
            },
        );
        let text = serde_json::to_string(&config).unwrap();
        let restored: AppConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(restored.cookie, config.cookie);
        assert_eq!(restored.locations["test"].longitude, "114.10");
    }

    #[test]
    fn malformed_text_falls_back_to_default() {
        let text = "not json at all";
        let config: AppConfig = serde_json::from_str(text).unwrap_or_default();
        assert!(config.cookie.is_empty());
    }
}
