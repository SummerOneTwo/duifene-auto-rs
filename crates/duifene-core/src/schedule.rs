//! 高强度轮询窗口调度:某门课在指定时间段内用短间隔(如 2s)高频探测,
//! 避免错过只开放一两分钟的签到;窗口外由 daemon 回到慢速保活轮询。

use chrono::{Datelike, NaiveDateTime, Timelike};
use serde::{Deserialize, Serialize};

use crate::models::Course;

fn default_interval_secs() -> u64 {
    2
}

fn default_warmup_minutes() -> u32 {
    0
}

/// 一个"高强度轮询窗口":当现在处于该窗口的星期与时间段内、且课程匹配
/// 任一被监控课程时,使用 `interval_secs` 作为轮询间隔。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PollWindow {
    /// 匹配课程:课程名包含该串,或与课堂ID/课程ID 完全相等;空串表示匹配任意课程
    #[serde(default)]
    pub course: String,
    /// 星期列表 1=周一 … 7=周日;空表示每天
    #[serde(default)]
    pub weekdays: Vec<u8>,
    /// 开始时刻 "HH:MM"(实际生效提前 warmup_minutes 分钟)
    #[serde(default)]
    pub start: String,
    /// 结束时刻 "HH:MM"(含该分钟,防止临界签到漏掉)
    #[serde(default)]
    pub end: String,
    /// 窗口内轮询间隔(秒),默认 2
    #[serde(default = "default_interval_secs")]
    pub interval_secs: u64,
    /// 提前多少分钟进入高强度轮询,默认 0
    #[serde(default = "default_warmup_minutes")]
    pub warmup_minutes: u32,
}

/// "HH:MM" 转当日分钟数;无效返回 None
pub fn parse_hhmm(text: &str) -> Option<u32> {
    let (hour, minute) = text.trim().split_once(':')?;
    let hour: u32 = hour.parse().ok()?;
    let minute: u32 = minute.parse().ok()?;
    if hour > 23 || minute > 59 {
        return None;
    }
    Some(hour * 60 + minute)
}

/// 星期文本转 1..=7(1=周一)。支持 "1"~"7"、"周一"~"周日/周天"、"mon"~"sun"。
pub fn weekday_from_text(text: &str) -> Option<u8> {
    let text = text.trim().to_lowercase();
    if let Ok(number) = text.parse::<u8>()
        && (1..=7).contains(&number)
    {
        return Some(number);
    }
    let number = match text.as_str() {
        "周一" | "星期一" | "mon" | "monday" => 1,
        "周二" | "星期二" | "tue" | "tues" | "tuesday" => 2,
        "周三" | "星期三" | "wed" | "wednesday" => 3,
        "周四" | "星期四" | "thu" | "thur" | "thursday" => 4,
        "周五" | "星期五" | "fri" | "friday" => 5,
        "周六" | "星期六" | "sat" | "saturday" => 6,
        "周日" | "周天" | "星期日" | "星期天" | "sun" | "sunday" => 7,
        _ => return None,
    };
    Some(number)
}

/// 星期数字(1=周一)转中文标签
pub fn weekday_label(weekday: u8) -> &'static str {
    match weekday {
        1 => "周一",
        2 => "周二",
        3 => "周三",
        4 => "周四",
        5 => "周五",
        6 => "周六",
        _ => "周日",
    }
}

impl PollWindow {
    pub fn is_valid(&self) -> bool {
        let Some(start) = parse_hhmm(&self.start) else {
            return false;
        };
        let Some(end) = parse_hhmm(&self.end) else {
            return false;
        };
        if end < start {
            return false;
        }
        self.weekdays.iter().all(|day| (1..=7).contains(day))
            && self.interval_secs >= 1
            && self.warmup_minutes <= 24 * 60
    }

    pub fn course_matches(&self, course: &Course) -> bool {
        if self.course.is_empty() {
            return true;
        }
        course.name.contains(self.course.as_str())
            || course.tclass_id == self.course
            || course.class_id == self.course
    }

    /// 给定星期(1=周一)与当日分钟数,窗口是否生效(含预热与结束分钟)
    pub fn active_at(&self, weekday: u8, minute_of_day: u32) -> bool {
        if !self.weekdays.is_empty() && !self.weekdays.contains(&weekday) {
            return false;
        }
        let Some(start) = parse_hhmm(&self.start) else {
            return false;
        };
        let Some(end) = parse_hhmm(&self.end) else {
            return false;
        };
        // warmup 与 minute_of_day 同为分钟
        let start = start.saturating_sub(self.warmup_minutes);
        minute_of_day >= start && minute_of_day <= end
    }
}

/// 在所有窗口里找出"当前正处于其时间段、且课程匹配任一被监控课程"的窗口,
/// 返回其中最小的轮询间隔秒数(多窗口重叠时取更密的那个)。
pub fn active_interval_secs(
    windows: &[PollWindow],
    watched: &[Course],
    now: NaiveDateTime,
) -> Option<u64> {
    let weekday = (now.weekday().num_days_from_monday() + 1) as u8;
    let minute_of_day = now.hour() * 60 + now.minute();
    windows
        .iter()
        .filter(|window| {
            window.active_at(weekday, minute_of_day)
                && watched.iter().any(|course| window.course_matches(course))
        })
        .map(|window| window.interval_secs)
        .min()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn friday_1355() -> NaiveDateTime {
        // 2026-09-11 是周五
        NaiveDate::from_ymd_opt(2026, 9, 11)
            .unwrap()
            .and_hms_opt(13, 55, 0)
            .unwrap()
    }

    fn window(friday_only: bool) -> PollWindow {
        PollWindow {
            course: "软件工程".to_string(),
            weekdays: if friday_only { vec![5] } else { vec![] },
            start: "13:55".to_string(),
            end: "17:15".to_string(),
            interval_secs: 2,
            warmup_minutes: 0,
        }
    }

    fn course() -> Course {
        Course {
            class_id: "101".to_string(),
            tclass_id: "c1".to_string(),
            name: "软件工程实践".to_string(),
        }
    }

    #[test]
    fn window_active_inside_range() {
        let win = window(true);
        let now = friday_1355();
        let weekday = (now.weekday().num_days_from_monday() + 1) as u8;
        assert_eq!(weekday, 5);
        assert!(win.active_at(weekday, 14 * 60 + 0));
        assert!(win.active_at(weekday, 17 * 60 + 15));
        assert!(!win.active_at(weekday, 17 * 60 + 16));
        assert!(!win.active_at(weekday, 13 * 60 + 54));
    }

    #[test]
    fn window_warmup_starts_early() {
        let mut win = window(true);
        win.warmup_minutes = 10;
        assert!(win.active_at(5, 13 * 60 + 50));
        assert!(win.active_at(5, 13 * 60 + 45));
        assert!(!win.active_at(5, 13 * 60 + 44));
    }

    #[test]
    fn weekday_filter_applies() {
        let win = window(true); // 仅周五
        assert!(win.active_at(5, 14 * 60));
        assert!(!win.active_at(4, 14 * 60)); // 周四不应触发
        let everyday = window(false);
        assert!(everyday.active_at(4, 14 * 60));
    }

    #[test]
    fn course_match_rules() {
        let win = window(true);
        let c = course();
        assert!(win.course_matches(&c)); // 课程名包含
        let by_tclass = Course {
            name: "别的课".to_string(),
            ..c.clone()
        };
        let tclass_win = PollWindow {
            course: "c1".to_string(),
            ..window(true)
        };
        assert!(tclass_win.course_matches(&by_tclass));
        assert!(!win.course_matches(&by_tclass));
    }

    #[test]
    fn active_interval_picks_fastest_overlap() {
        let mut fast = window(true);
        fast.interval_secs = 2;
        let mut slow = window(true);
        slow.interval_secs = 30;
        let now = friday_1355();
        let interval = active_interval_secs(&[slow.clone(), fast.clone()], &[course()], now);
        assert_eq!(interval, Some(2));
        // 时间在窗口外 => 无快速轮询
        let outside = now + chrono::Duration::hours(5);
        assert_eq!(
            active_interval_secs(&[slow, fast.clone()], &[course()], outside),
            None
        );
        // 课程不匹配 => 不触发
        let other_course = Course {
            name: "高数".to_string(),
            tclass_id: "c9".to_string(),
            class_id: "909".to_string(),
        };
        assert_eq!(
            active_interval_secs(&[fast], &[other_course], now),
            None
        );
    }

    #[test]
    fn parse_weekday_text() {
        assert_eq!(weekday_from_text("5"), Some(5));
        assert_eq!(weekday_from_text("周五"), Some(5));
        assert_eq!(weekday_from_text("星期五"), Some(5));
        assert_eq!(weekday_from_text("fri"), Some(5));
        assert_eq!(weekday_from_text("Friday"), Some(5));
        assert_eq!(weekday_from_text("周一"), Some(1));
        assert_eq!(weekday_from_text("周天"), Some(7));
        assert_eq!(weekday_from_text("8"), None);
        assert_eq!(weekday_from_text("周日"), Some(7));
        assert_eq!(weekday_from_text("周六"), Some(6));
    }

    #[test]
    fn time_parsing_and_validation() {
        assert_eq!(parse_hhmm("13:55"), Some(13 * 60 + 55));
        assert_eq!(parse_hhmm("00:00"), Some(0));
        assert_eq!(parse_hhmm("24:00"), None);
        assert_eq!(parse_hhmm("13:60"), None);
        assert!(window(true).is_valid());
        let mut bad = window(true);
        bad.end = "13:00".to_string();
        assert!(!bad.is_valid());
        let mut zero = window(true);
        zero.interval_secs = 0;
        assert!(!zero.is_valid());
    }
}
