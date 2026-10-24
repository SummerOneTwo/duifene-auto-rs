use std::collections::{BTreeMap, HashMap};
use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::email::MailConfig;
use crate::schedule::PollWindow;

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
    /// 按课程覆盖的门槛:键为课程名或课堂ID,命中则覆盖全局 signed_percent
    #[serde(default)]
    pub signed_percent_overrides: BTreeMap<String, u32>,
    /// SMTP 邮件通知配置(登录态掉线/签到成功/失败)
    #[serde(default)]
    pub mail: Option<MailConfig>,
    /// 高强度轮询窗口:窗口内的课程用短间隔轮询,窗口外回到慢速保活
    #[serde(default)]
    pub poll_windows: Vec<PollWindow>,
    /// 已处理完毕的活动ID:签过/已结束/已通知失败的,重启后不再重新发现或重发邮件
    #[serde(default)]
    pub handled: Vec<String>,
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

/// 保存某门课的签到门槛覆盖(0-100);传 None 表示移除该课覆盖,回到全局值
pub fn save_course_percent(course: &str, percent: Option<u32>) -> io::Result<()> {
    let mut config = load_config();
    match percent {
        Some(value) => {
            config
                .signed_percent_overrides
                .insert(course.to_string(), value.clamp(0, 100));
        }
        None => {
            config.signed_percent_overrides.remove(course);
        }
    }
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

/// 供命令行显示当前配置文件路径
pub fn config_path_display() -> String {
    config_path().display().to_string()
}

fn write_config(config: &AppConfig) -> io::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(config).map_err(io::Error::other)?;
    fs::write(&path, text)?;
    // 配置含 cookie 与邮箱授权码,限制为本用户可读写
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Ok(())
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

/// 保存 SMTP 邮件通知配置;传入 None 表示清除邮件配置
pub fn save_mail_config(mail: Option<MailConfig>) -> io::Result<()> {
    let mut config = load_config();
    config.mail = mail;
    write_config(&config)
}

/// 覆盖保存高强度轮询窗口列表
pub fn save_poll_windows(windows: &[PollWindow]) -> io::Result<()> {
    let mut config = load_config();
    config.poll_windows = windows.to_vec();
    write_config(&config)
}

/// 追加一个"已处理"的活动ID(幂等;失败静默,不影响主流程)
pub fn add_handled_activity(activity_id: &str) {
    let mut config = load_config();
    if config.handled.iter().any(|id| id == activity_id) || activity_id.is_empty() {
        return;
    }
    config.handled.push(activity_id.to_string());
    // 避免无限膨胀:只保留最近 500 条
    if config.handled.len() > 500 {
        let keep = config.handled.len() - 500;
        config.handled.drain(..keep);
    }
    let _ = write_config(&config);
}

/// 已处理活动集合(用于跳过重启后重复发现/重发)
pub fn handled_activities() -> std::collections::HashSet<String> {
    load_config()
        .handled
        .into_iter()
        .collect::<std::collections::HashSet<String>>()
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

    #[test]
    fn signed_percent_overrides_round_trip() {
        let mut config = AppConfig {
            signed_percent: 30,
            ..Default::default()
        };
        config
            .signed_percent_overrides
            .insert("软件工程".to_string(), 50);
        let text = serde_json::to_string(&config).unwrap();
        let restored: AppConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(restored.signed_percent, 30);
        assert_eq!(restored.signed_percent_overrides["软件工程"], 50);
        // 没配覆盖的课回退到全局
        assert_eq!(
            restored
                .signed_percent_overrides
                .get("签到测试")
                .copied()
                .unwrap_or(restored.signed_percent),
            30
        );
    }

    #[test]
    fn old_config_without_overrides_deserializes() {
        // 兼容旧配置(无 signed_percent_overrides 字段)
        let text = r#"{"cookie":"c","signed_percent":0}"#;
        let config: AppConfig = serde_json::from_str(text).unwrap();
        assert!(config.signed_percent_overrides.is_empty());
    }
}
