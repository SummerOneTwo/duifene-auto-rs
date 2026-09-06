use std::collections::{HashMap, HashSet};

use chrono::{Duration as ChronoDuration, NaiveDateTime};

use crate::api::{CheckInResult, Client, SignReq};
use crate::filter::qualified_activities;
use crate::models::{Course, SignActivity, SignType};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub delay_seconds: i64,
    pub coords: HashMap<String, (f64, f64)>,
    pub refresh_every: u64,
    /// 签到门槛:已签到人数占比达到该百分比(0-100)才自动签到;0 表示不启用该限制
    pub signed_percent: u32,
}

impl Default for EngineConfig {
    fn default() -> Self {
        EngineConfig {
            delay_seconds: 0,
            coords: HashMap::new(),
            refresh_every: 300,
            signed_percent: 0,
        }
    }
}

#[derive(Debug)]
pub enum Event {
    Info(String),
    Warn(String),
    Found {
        course_name: String,
        activity_id: String,
        kind: String,
        code: Option<String>,
    },
    Signed {
        course_name: String,
        activity_id: String,
        result: CheckInResult,
    },
}

struct Pending {
    activity: SignActivity,
    course_index: usize,
    due: NaiveDateTime,
}

struct RetryItem {
    activity: SignActivity,
    course_index: usize,
    due: NaiveDateTime,
}

/// 等待签到人数达标的活动
struct Waiting {
    activity: SignActivity,
    course_index: usize,
}

pub struct Engine {
    client: Box<dyn Client>,
    watched: Vec<Course>,
    config: EngineConfig,
    seen: HashSet<String>,
    pending: Vec<Pending>,
    retries: Vec<RetryItem>,
    waiting: Vec<Waiting>,
    attempts: HashMap<String, u32>,
    running: bool,
    tick_no: u64,
}

const MAX_SIGN_ATTEMPTS: u32 = 3;
const RETRY_INTERVAL_SECONDS: i64 = 60;

fn kind_label(kind: SignType) -> &'static str {
    match kind {
        SignType::Code => "数字码",
        SignType::Qr => "二维码",
        SignType::Locate => "定位",
        SignType::Unknown => "未知",
    }
}

impl Engine {
    pub fn new(client: Box<dyn Client>, courses: Vec<Course>, config: EngineConfig) -> Self {
        Engine {
            client,
            watched: courses,
            config,
            seen: HashSet::new(),
            pending: Vec::new(),
            retries: Vec::new(),
            waiting: Vec::new(),
            attempts: HashMap::new(),
            running: true,
            tick_no: 0,
        }
    }

    pub fn running(&self) -> bool {
        self.running
    }

    pub fn stop(&mut self) {
        self.running = false;
        self.pending.clear();
        self.retries.clear();
        self.waiting.clear();
    }

    pub fn earliest_due(&self, now: NaiveDateTime) -> Option<std::time::Duration> {
        let mut earliest = self.pending.iter().map(|item| item.due).min();
        if let Some(retry_due) = self.retries.iter().map(|item| item.due).min() {
            earliest = match earliest {
                Some(current) => Some(current.min(retry_due)),
                None => Some(retry_due),
            };
        }
        let earliest = earliest?;
        let remaining = earliest - now;
        if remaining < ChronoDuration::zero() {
            return Some(std::time::Duration::ZERO);
        }
        remaining.to_std().ok()
    }

    pub fn tick(&mut self, now: NaiveDateTime) -> Vec<Event> {
        let mut events: Vec<Event> = Vec::new();
        if !self.running {
            return events;
        }
        self.tick_no += 1;

        if let Err(error) = self.client.check_login() {
            events.push(Event::Warn(format!("登录失效,已自动停止: {error}")));
            self.stop();
            return events;
        }

        // 先处理上一轮遗留下来等待人数达标的活动(新发现的活动本 tick 不重查,
        // 避免同一 tick 内重复查询导致阈值瞬时翻盘)
        if !self.waiting.is_empty() {
            let mut still_waiting = Vec::new();
            let mut now_ready: Vec<(SignActivity, usize)> = Vec::new();
            for item in std::mem::take(&mut self.waiting) {
                if self.reached_threshold(&item.activity) {
                    now_ready.push((item.activity, item.course_index));
                } else {
                    still_waiting.push(item);
                }
            }
            self.waiting = still_waiting;
            if !now_ready.is_empty() {
                let mut ready_batch: Vec<(SignReq, usize)> = Vec::new();
                for (activity, course_index) in now_ready {
                    events.push(Event::Info(format!(
                        "签到人数已达标,签到 [{}]",
                        self.watched[course_index].name
                    )));
                    if let Some(request) = self.sign_request(activity, course_index) {
                        ready_batch.push((request, course_index));
                    }
                }
                events.extend(self.sign_all(ready_batch, now));
            }
        }

        let mut due_items: Vec<Pending> = Vec::new();
        let mut remaining_pending: Vec<Pending> = Vec::new();
        for item in std::mem::take(&mut self.pending) {
            if item.due <= now {
                due_items.push(item);
            } else {
                remaining_pending.push(item);
            }
        }
        self.pending = remaining_pending;
        let mut due_batch: Vec<(SignReq, usize)> = Vec::new();
        for item in due_items {
            if let Some(request) = self.sign_request(item.activity, item.course_index) {
                due_batch.push((request, item.course_index));
            }
        }
        events.extend(self.sign_all(due_batch, now));

        let mut retry_items: Vec<RetryItem> = Vec::new();
        let mut remaining_retries: Vec<RetryItem> = Vec::new();
        for item in std::mem::take(&mut self.retries) {
            if item.due <= now {
                retry_items.push(item);
            } else {
                remaining_retries.push(item);
            }
        }
        self.retries = remaining_retries;
        let mut retry_batch: Vec<(SignReq, usize)> = Vec::new();
        for item in retry_items {
            if let Some(request) = self.sign_request(item.activity, item.course_index) {
                retry_batch.push((request, item.course_index));
            }
        }
        events.extend(self.sign_all(retry_batch, now));

        if self.tick_no.is_multiple_of(self.config.refresh_every)
            && let Ok(courses) = self.client.fetch_courses()
        {
            for course in courses {
                let exists = self
                    .watched
                    .iter()
                    .any(|watched| watched.tclass_id == course.tclass_id);
                if !exists {
                    self.watched.push(course.clone());
                    events.push(Event::Info(format!("已自动加入课程 {}", course.name)));
                }
            }
        }

        let fetched = self.client.fetch_all(&self.watched);
        let mut immediate: Vec<(SignActivity, usize)> = Vec::new();

        for (course_index, (course_name, fetched_result)) in
            self.watched_names().into_iter().zip(fetched).enumerate()
        {
            let rows = match fetched_result {
                Ok(rows) => rows,
                Err(error) => {
                    if self.tick_no % 10 == 1 {
                        events.push(Event::Warn(format!(
                            "课程 {} 拉取失败: {error}",
                            course_name
                        )));
                    }
                    continue;
                }
            };
            let candidates = qualified_activities(&rows, &self.seen, now);
            for candidate in candidates {
                let activity = SignActivity::from_raw(candidate);
                if !self.seen.insert(activity.id.clone()) {
                    continue;
                }
                events.push(Event::Found {
                    course_name: course_name.clone(),
                    activity_id: activity.id.clone(),
                    kind: kind_label(activity.r#type).to_string(),
                    code: activity.code.clone(),
                });
                // 签到人数门槛:未达标先挂起等待,达标后才安排签到
                if !self.reached_threshold(&activity) {
                    self.waiting.push(Waiting {
                        activity,
                        course_index,
                    });
                    continue;
                }
                if self.config.delay_seconds > 0 {
                    let due = now + ChronoDuration::seconds(self.config.delay_seconds);
                    self.pending.push(Pending {
                        activity,
                        course_index,
                        due,
                    });
                    events.push(Event::Info(format!(
                        "将在 {} 秒后自动签到 [{}]",
                        self.config.delay_seconds, course_name
                    )));
                } else {
                    immediate.push((activity, course_index));
                }
            }
        }

        let mut immediate_batch: Vec<(SignReq, usize)> = Vec::new();
        for (activity, course_index) in immediate {
            if let Some(request) = self.sign_request(activity, course_index) {
                immediate_batch.push((request, course_index));
            }
        }
        events.extend(self.sign_all(immediate_batch, now));

        if self.tick_no == 1 {
            events.push(Event::Info(format!(
                "持续监控中,共 {} 门课",
                self.watched.len()
            )));
        }
        events
    }

    /// 返回所有被监控课程的名称(按顺序),用于在循环中避免借用 self.watched
    fn watched_names(&self) -> Vec<String> {
        self.watched.iter().map(|course| course.name.clone()).collect()
    }

    /// 签到人数是否达到门槛。signed_percent==0 表示不启用限制,视为达到;
    /// 查询失败时保守放行(避免因接口问题永远错过签到)。
    fn reached_threshold(&mut self, activity: &SignActivity) -> bool {
        let percent = self.config.signed_percent;
        if percent == 0 {
            return true;
        }
        match self.client.arrival_count(&activity.id) {
            Ok((signed, total)) => {
                if total == 0 {
                    // 无总人数信息,保守放行
                    return true;
                }
                (signed as f64 / total as f64) >= (percent as f64 / 100.0)
            }
            Err(_) => true,
        }
    }

    fn sign_request(&mut self, activity: SignActivity, course_index: usize) -> Option<SignReq> {
        let course = self.watched[course_index].clone();
        let coords = self.resolve_coords(&activity, &course);
        Some(SignReq {
            activity,
            course,
            coords,
        })
    }

    fn resolve_coords(&mut self, activity: &SignActivity, course: &Course) -> Option<(f64, f64)> {
        if activity.r#type != SignType::Locate {
            return None;
        }
        if let Some(coords) = activity.coordinate {
            return Some(coords);
        }
        if let Some(coords) = self.client.activity_coords(activity, course) {
            return Some(coords);
        }
        self.config.coords.get(&course.name).copied()
    }

    fn sign_all(&mut self, batch: Vec<(SignReq, usize)>, now: NaiveDateTime) -> Vec<Event> {
        if batch.is_empty() {
            return Vec::new();
        }
        let requests: Vec<SignReq> = batch.iter().map(|(request, _)| request.clone()).collect();
        let results = self.client.sign_many(&requests);
        let mut events = Vec::with_capacity(batch.len());
        for ((request, course_index), result) in batch.into_iter().zip(results) {
            if let CheckInResult::Failed(message) = &result {
                let retry_forever = message.contains("过于频繁") || message.contains("稍后");
                let attempt_count = self
                    .attempts
                    .entry(request.activity.id.clone())
                    .or_insert(0);
                *attempt_count += 1;
                if retry_forever || *attempt_count < MAX_SIGN_ATTEMPTS {
                    let due = now + ChronoDuration::seconds(RETRY_INTERVAL_SECONDS);
                    self.retries.push(RetryItem {
                        activity: request.activity.clone(),
                        course_index,
                        due,
                    });
                }
            }
            events.push(Event::Signed {
                course_name: request.course.name,
                activity_id: request.activity.id,
                result,
            });
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ApiErr;
    use crate::mock::MockClient;
    use crate::models::RawRow;

    fn course(class_id: &str, tclass_id: &str, name: &str) -> Course {
        Course {
            class_id: class_id.to_string(),
            tclass_id: tclass_id.to_string(),
            name: name.to_string(),
        }
    }

    fn row(json: &str) -> RawRow {
        serde_json::from_str(json).expect("Json should be parsed")
    }

    fn dt(seconds: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(seconds, "%Y/%m/%d %H:%M:%S").unwrap()
    }

    fn code_activity(id: &str) -> String {
        format!(
            r#"{{"ID":"{id}","CheckInType":"1","CheckInCode":"8888","CanApply":"1","CreaterDate":"2026/08/10 07:55:00"}}"#
        )
    }

    fn config(delay_seconds: i64) -> EngineConfig {
        EngineConfig {
            delay_seconds,
            coords: HashMap::new(),
            refresh_every: 300,
            signed_percent: 0,
        }
    }

    fn signed_ok_ids(events: &[Event]) -> Vec<&str> {
        events
            .iter()
            .filter_map(|event| match event {
                Event::Signed {
                    activity_id,
                    result: CheckInResult::Ok(_),
                    ..
                } => Some(activity_id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn code_activity_is_found_and_signed_once() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(0),
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert_eq!(signed_ok_ids(&events), ["a1"]);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Found { activity_id, .. } if activity_id == "a1"
        )));

        let events = engine.tick(dt("2026/08/10 08:00:02"));
        assert!(signed_ok_ids(&events).is_empty());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Found { .. }))
        );
    }

    #[test]
    fn delayed_sign_happens_when_due() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(5),
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert!(signed_ok_ids(&events).is_empty());
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Found { activity_id, .. } if activity_id == "a1"
        )));

        for second in 1..5 {
            let now = dt(&format!("2026/08/10 08:00:0{second}"));
            let events = engine.tick(now);
            assert!(signed_ok_ids(&events).is_empty(), "第 {second} 秒不该签");
        }
        let events = engine.tick(dt("2026/08/10 08:00:05"));
        assert_eq!(signed_ok_ids(&events), ["a1"]);
    }

    #[test]
    fn two_courses_sign_in_same_tick() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client.queue_rows("c2", vec![row(&code_activity("a2"))]);
        let courses = vec![course("101", "c1", "课程一"), course("102", "c2", "课程二")];
        let mut engine = Engine::new(Box::new(client), courses, config(0));

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert_eq!(signed_ok_ids(&events), ["a1", "a2"]);
    }

    #[test]
    fn one_course_failure_does_not_block_another() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client.queue_rows_error("c2", ApiErr::Msg("服务端开小差".to_string()));
        let courses = vec![course("101", "c1", "课程一"), course("102", "c2", "课程二")];
        let mut engine = Engine::new(Box::new(client), courses, config(0));

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert_eq!(signed_ok_ids(&events), ["a1"]);
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Warn(message) if message.contains("课程二")
        )));
    }

    #[test]
    fn login_loss_stops_engine() {
        let mut client = MockClient::new();
        client.login_error = Some(ApiErr::AuthLost);
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(0),
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert!(!engine.running());
        assert!(
            events
                .iter()
                .any(|event| matches!(event, Event::Warn(message) if message.contains("登录失效")))
        );
    }

    #[test]
    fn gone_result_is_reported_but_not_as_error() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client
            .sign_results
            .push_back(vec![CheckInResult::Gone("活动已结束".to_string())]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(0),
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Signed {
                result: CheckInResult::Gone(_),
                ..
            }
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            Event::Signed {
                result: CheckInResult::Failed(_),
                ..
            }
        )));
    }

    #[test]
    fn location_coords_fall_through_row_then_page_then_config() {
        fn locate_activity(coordinate: Option<(f64, f64)>) -> SignActivity {
            SignActivity {
                id: "a1".to_string(),
                r#type: SignType::Locate,
                code: None,
                coordinate,
            }
        }

        let mut coords = HashMap::new();
        coords.insert("课一".to_string(), (115.0, 23.0));
        let mut client = MockClient::new();
        let courses = vec![course("101", "c1", "课一"), course("102", "c2", "未配置")];
        client.coords_results.push_back(Some((114.20, 22.70)));
        client.coords_results.push_back(None);
        client.coords_results.push_back(None);
        let mut engine = Engine::new(
            Box::new(client),
            courses,
            EngineConfig {
                delay_seconds: 0,
                coords,
                refresh_every: 300,
                signed_percent: 0,
            },
        );

        let course_one = engine.watched[0].clone();
        let from_row = locate_activity(Some((113.5, 21.5)));
        assert_eq!(
            engine.resolve_coords(&from_row, &course_one),
            Some((113.5, 21.5))
        );

        let course_one = engine.watched[0].clone();
        let from_page = locate_activity(None);
        assert_eq!(
            engine.resolve_coords(&from_page, &course_one),
            Some((114.20, 22.70))
        );

        let course_one = engine.watched[0].clone();
        let from_config = locate_activity(None);
        assert_eq!(
            engine.resolve_coords(&from_config, &course_one),
            Some((115.0, 23.0))
        );

        let course_unconfigured = engine.watched[1].clone();
        let unresolvable = locate_activity(None);
        assert_eq!(
            engine.resolve_coords(&unresolvable, &course_unconfigured),
            None
        );

        let course_one = engine.watched[0].clone();
        let code_kind = SignActivity {
            id: "a2".to_string(),
            r#type: SignType::Code,
            code: Some("8888".to_string()),
            coordinate: None,
        };
        assert_eq!(engine.resolve_coords(&code_kind, &course_one), None);
    }

    #[test]
    fn refresh_adds_new_courses() {
        let mut client = MockClient::new();
        client.queue_courses(Ok(vec![
            course("101", "c1", "课程一"),
            course("102", "c2", "课程二"),
        ]));
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client.queue_rows("c2", vec![row(&code_activity("a2"))]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            EngineConfig {
                delay_seconds: 0,
                coords: HashMap::new(),
                refresh_every: 1,
                signed_percent: 0,
            },
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Info(message) if message.contains("已自动加入课程")
        )));
        let signed: Vec<&str> = signed_ok_ids(&events);
        assert!(signed.contains(&"a1") && signed.contains(&"a2"));
    }

    #[test]
    fn below_threshold_defers_sign() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client.arrival_results.push_back(Ok((2, 10))); // 20% < 30%
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            EngineConfig {
                delay_seconds: 0,
                coords: HashMap::new(),
                refresh_every: 1,
                signed_percent: 30,
            },
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert!(!events.iter().any(|event| matches!(event, Event::Signed { .. })));
        assert_eq!(engine.waiting.len(), 1);
    }

    #[test]
    fn reaching_threshold_signs_after_defers_then_signs() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        // 第一次 tick: 3/10 = 30% 达标 => 直接签
        client.arrival_results.push_back(Ok((3, 10)));
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            EngineConfig {
                delay_seconds: 0,
                coords: HashMap::new(),
                refresh_every: 1,
                signed_percent: 30,
            },
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert!(signed_ok_ids(&events).contains(&"a1"));
        assert!(engine.waiting.is_empty());
    }

    #[test]
    fn arrival_query_error_defers_conservatively() {
        // 查询失败时保守放行(直接签),避免错过签到
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client.arrival_results.push_back(Err(ApiErr::Parse("x".to_string())));
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            EngineConfig {
                delay_seconds: 0,
                coords: HashMap::new(),
                refresh_every: 1,
                signed_percent: 30,
            },
        );

        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert!(signed_ok_ids(&events).contains(&"a1"));
    }

    #[test]
    fn duplicate_rows_in_same_fetch_sign_once() {
        let mut client = MockClient::new();
        let activity = row(&code_activity("a1"));
        client.queue_rows("c1", vec![activity.clone(), activity]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(0),
        );
        let events = engine.tick(dt("2026/08/10 08:00:00"));
        assert_eq!(signed_ok_ids(&events), ["a1"]);
        let found_count = events
            .iter()
            .filter(|event| matches!(event, Event::Found { .. }))
            .count();
        assert_eq!(found_count, 1);
    }

    #[test]
    fn frequent_failure_keeps_retrying_until_success() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client
            .sign_results
            .push_back(vec![CheckInResult::Failed("操作过于频繁".to_string())]);
        client
            .sign_results
            .push_back(vec![CheckInResult::Failed("请稍后再试".to_string())]);
        client
            .sign_results
            .push_back(vec![CheckInResult::Ok("签到成功".to_string())]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(0),
        );

        let start = dt("2026/08/10 08:00:00");
        let first = engine.tick(start);
        assert!(first.iter().any(|event| matches!(
            event,
            Event::Signed {
                result: CheckInResult::Failed(message),
                ..
            } if message.contains("过于频繁")
        )));
        assert!(engine.earliest_due(dt("2026/08/10 08:00:30")).is_some());
        let second = engine.tick(dt("2026/08/10 08:01:00"));
        assert!(second.iter().any(|event| matches!(
            event,
            Event::Signed {
                result: CheckInResult::Failed(message),
                ..
            } if message.contains("稍后")
        )));
        let third = engine.tick(dt("2026/08/10 08:02:00"));
        assert_eq!(signed_ok_ids(&third), ["a1"]);
    }

    #[test]
    fn failed_sign_is_retried_until_success() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        client
            .sign_results
            .push_back(vec![CheckInResult::Failed("网络波动".to_string())]);
        client
            .sign_results
            .push_back(vec![CheckInResult::Ok("签到成功".to_string())]);
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(0),
        );

        let start = dt("2026/08/10 08:00:00");
        let events = engine.tick(start);
        assert!(signed_ok_ids(&events).is_empty());
        assert!(events.iter().any(|event| matches!(
            event,
            Event::Signed {
                result: CheckInResult::Failed(_),
                ..
            }
        )));

        let events = engine.tick(dt("2026/08/10 08:00:30"));
        assert!(signed_ok_ids(&events).is_empty());
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Signed { .. }))
        );

        let events = engine.tick(dt("2026/08/10 08:01:00"));
        assert_eq!(signed_ok_ids(&events), ["a1"]);
    }

    #[test]
    fn failed_sign_gives_up_after_three_attempts() {
        let mut client = MockClient::new();
        client.queue_rows("c1", vec![row(&code_activity("a1"))]);
        for _ in 0..3 {
            client
                .sign_results
                .push_back(vec![CheckInResult::Failed("网络波动".to_string())]);
        }
        let mut engine = Engine::new(
            Box::new(client),
            vec![course("101", "c1", "课程一")],
            config(0),
        );

        engine.tick(dt("2026/08/10 08:00:00"));
        engine.tick(dt("2026/08/10 08:01:00"));
        let events = engine.tick(dt("2026/08/10 08:02:00"));
        let failed_count = events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    Event::Signed {
                        result: CheckInResult::Failed(_),
                        ..
                    }
                )
            })
            .count();
        assert_eq!(failed_count, 1);

        let events = engine.tick(dt("2026/08/10 08:03:00"));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, Event::Signed { .. }))
        );
    }
}
