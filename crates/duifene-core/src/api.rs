use thiserror::Error;

use crate::models::{Course, RawRow, SignActivity};

#[derive(Debug, Clone, Error)]
pub enum ApiErr {
    #[error("登录已失效")]
    AuthLost,
    #[error("网络错误: {0}")]
    Network(String),
    #[error("HTTP 错误: {0}")]
    Http(u16),
    #[error("响应解析失败: {0}")]
    Parse(String),
    #[error("服务器拒绝: {0}")]
    Msg(String),
}

#[derive(Debug)]
pub enum CheckInResult {
    Ok(String),
    Gone(String),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct SignReq {
    pub activity: SignActivity,
    pub course: Course,
    pub coords: Option<(f64, f64)>,
}

pub trait Client {
    fn check_login(&mut self) -> Result<(), ApiErr>;
    fn fetch_courses(&mut self) -> Result<Vec<Course>, ApiErr>;
    fn fetch_all(&mut self, courses: &[Course]) -> Vec<Result<Vec<RawRow>, ApiErr>>;
    fn activity_coords(&mut self, activity: &SignActivity, course: &Course) -> Option<(f64, f64)>;
    fn sign_many(&mut self, requests: &[SignReq]) -> Vec<CheckInResult>;
    /// 查询某签到活动的已签到人数与总人数;返回 (已签到人数, 总人数)
    fn arrival_count(&mut self, activity_id: &str) -> Result<(u32, u32), ApiErr>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_is_object_safe() {
        let _assert: Option<&mut dyn Client> = None;
    }
}
