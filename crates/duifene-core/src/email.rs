use std::time::Duration;

use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::transport::smtp::client::{Tls, TlsParameters};
use lettre::transport::smtp::SmtpTransport;
use lettre::{Message, Transport};
use serde::{Deserialize, Serialize};

/// QQ 邮箱默认 SMTP 服务器(SSL 直连端口)
pub const QQ_SMTP_HOST: &str = "smtp.qq.com";
pub const QQ_SMTP_PORT: u16 = 465;

/// SMTP 发信配置。username 通常是发件邮箱本身, auth_code 是邮箱服务商
/// 提供的客户端授权码(不是登录密码)。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MailConfig {
    #[serde(default = "default_smtp_host")]
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    pub username: String,
    pub auth_code: String,
    /// 发件人地址;缺省时用 username
    #[serde(default)]
    pub from: Option<String>,
    /// 收件人地址;缺省时用 username(发给自己)
    #[serde(default)]
    pub to: Option<String>,
}

fn default_smtp_host() -> String {
    QQ_SMTP_HOST.to_string()
}

fn default_smtp_port() -> u16 {
    QQ_SMTP_PORT
}

impl MailConfig {
    pub fn qq(username: String, auth_code: String) -> Self {
        MailConfig {
            host: QQ_SMTP_HOST.to_string(),
            port: QQ_SMTP_PORT,
            username,
            auth_code,
            from: None,
            to: None,
        }
    }

    pub fn recipient(&self) -> &str {
        self.to.as_deref().unwrap_or(self.username.as_str())
    }

    pub fn sender(&self) -> &str {
        self.from.as_deref().unwrap_or(self.username.as_str())
    }

    pub fn is_complete(&self) -> bool {
        !self.host.is_empty()
            && self.port > 0
            && !self.username.is_empty()
            && !self.auth_code.is_empty()
            && !self.sender().is_empty()
            && !self.recipient().is_empty()
    }

    /// 直接连接 SMTP 服务器发送一封邮件。每次调用独立建连,适合低频通知。
    pub fn send(&self, subject: &str, text: &str) -> Result<(), String> {
        let from: Mailbox = self
            .sender()
            .parse()
            .map_err(|error| format!("发件地址无效: {error}"))?;
        let to: Mailbox = self
            .recipient()
            .parse()
            .map_err(|error| format!("收件地址无效: {error}"))?;
        let email = Message::builder()
            .from(from)
            .to(to)
            .subject(subject)
            .body(text.to_string())
            .map_err(|error| format!("构建邮件失败: {error}"))?;

        let tls = TlsParameters::new(self.host.clone())
            .map_err(|error| format!("TLS 初始化失败: {error}"))?;
        let mailer = SmtpTransport::builder_dangerous(self.host.clone())
            .port(self.port)
            .tls(Tls::Wrapper(tls))
            .credentials(Credentials::new(self.username.clone(), self.auth_code.clone()))
            .timeout(Some(Duration::from_secs(20)))
            .build();

        mailer
            .send(&email)
            .map_err(|error| format!("SMTP 发送失败: {error}"))?;
        Ok(())
    }

    /// 带一次重试的发送(网络抖动时更稳),仍失败则返回错误字符串。
    pub fn send_with_retry(&self, subject: &str, text: &str) -> Result<(), String> {
        match self.send(subject, text) {
            Ok(()) => Ok(()),
            Err(first_error) => {
                std::thread::sleep(Duration::from_secs(3));
                self.send(subject, text)
                    .map_err(|second_error| format!("{first_error}; 重试仍失败: {second_error}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qq_defaults_are_sane() {
        let mail = MailConfig::qq("12345@qq.com".to_string(), "secret".to_string());
        assert_eq!(mail.host, "smtp.qq.com");
        assert_eq!(mail.port, 465);
        assert_eq!(mail.sender(), "12345@qq.com");
        assert_eq!(mail.recipient(), "12345@qq.com");
        assert!(mail.is_complete());
    }

    #[test]
    fn from_and_to_override_defaults() {
        let mut mail = MailConfig::qq("from@qq.com".to_string(), "x".to_string());
        mail.from = Some("自动签到 <no-reply@qq.com>".to_string());
        mail.to = Some("boss@qq.com".to_string());
        assert_eq!(mail.sender(), "自动签到 <no-reply@qq.com>");
        assert_eq!(mail.recipient(), "boss@qq.com");
    }

    #[test]
    fn incomplete_config_is_detected() {
        let mut mail = MailConfig::qq(String::new(), "x".to_string());
        assert!(!mail.is_complete());
        mail.username = "a@qq.com".to_string();
        mail.auth_code.clear();
        assert!(!mail.is_complete());
    }

    #[test]
    fn serde_round_trip_preserves_all_fields() {
        let mut mail = MailConfig::qq("user@qq.com".to_string(), "code".to_string());
        mail.to = Some("other@163.com".to_string());
        let text = serde_json::to_string(&mail).unwrap();
        let restored: MailConfig = serde_json::from_str(&text).unwrap();
        assert_eq!(restored.host, mail.host);
        assert_eq!(restored.port, mail.port);
        assert_eq!(restored.username, mail.username);
        assert_eq!(restored.auth_code, mail.auth_code);
        assert_eq!(restored.to, mail.to);
    }

    #[test]
    fn mailboxes_parse_from_bare_and_named() {
        let bare: Result<Mailbox, _> = "a@qq.com".parse();
        assert!(bare.is_ok());
        let named: Result<Mailbox, _> = "对分易 <a@qq.com>".parse();
        assert!(named.is_ok());
    }
}
