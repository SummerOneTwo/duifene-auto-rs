use std::collections::{HashMap, VecDeque};

use crate::api::{ApiErr, CheckInResult, Client, SignReq};
use crate::models::{Course, RawRow};

pub(crate) struct MockClient {
    pub login_error: Option<ApiErr>,
    pub login_calls: usize,
    pub course_lists: VecDeque<Result<Vec<Course>, ApiErr>>,
    pub rows_by_class: HashMap<String, VecDeque<Result<Vec<RawRow>, ApiErr>>>,
    pub fetch_calls: Vec<Vec<String>>,
    pub sign_results: VecDeque<Vec<CheckInResult>>,
    pub sign_requests: Vec<Vec<SignReq>>,
    pub coords_results: VecDeque<Option<(f64, f64)>>,
    pub coords_calls: usize,
    /// 签到人数查询返回队列:(已签到, 总人数);为空默认返回 (100,100) 放行
    pub arrival_results: VecDeque<Result<(u32, u32), ApiErr>>,
    pub arrival_calls: usize,
}

impl MockClient {
    pub fn new() -> Self {
        MockClient {
            login_error: None,
            login_calls: 0,
            course_lists: VecDeque::new(),
            rows_by_class: HashMap::new(),
            fetch_calls: Vec::new(),
            sign_results: VecDeque::new(),
            sign_requests: Vec::new(),
            coords_results: VecDeque::new(),
            coords_calls: 0,
            arrival_results: VecDeque::new(),
            arrival_calls: 0,
        }
    }

    pub fn queue_rows(&mut self, class_id: &str, rows: Vec<RawRow>) {
        self.rows_by_class
            .entry(class_id.to_string())
            .or_default()
            .push_back(Ok(rows));
    }

    pub fn queue_rows_error(&mut self, class_id: &str, error: ApiErr) {
        self.rows_by_class
            .entry(class_id.to_string())
            .or_default()
            .push_back(Err(error));
    }

    pub fn queue_courses(&mut self, courses: Result<Vec<Course>, ApiErr>) {
        self.course_lists.push_back(courses);
    }
}

impl Default for MockClient {
    fn default() -> Self {
        MockClient::new()
    }
}

impl Client for MockClient {
    fn check_login(&mut self) -> Result<(), ApiErr> {
        self.login_calls += 1;
        match &self.login_error {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }

    fn fetch_courses(&mut self) -> Result<Vec<Course>, ApiErr> {
        self.course_lists.pop_front().unwrap_or(Ok(Vec::new()))
    }

    fn fetch_all(&mut self, courses: &[Course]) -> Vec<Result<Vec<RawRow>, ApiErr>> {
        let class_ids: Vec<String> = courses
            .iter()
            .map(|course| course.tclass_id.clone())
            .collect();
        self.fetch_calls.push(class_ids.clone());
        courses
            .iter()
            .map(|course| {
                self.rows_by_class
                    .get_mut(&course.tclass_id)
                    .and_then(|queue| queue.pop_front())
                    .unwrap_or(Ok(Vec::new()))
            })
            .collect()
    }

    fn activity_coords(
        &mut self,
        _activity: &crate::models::SignActivity,
        _course: &Course,
    ) -> Option<(f64, f64)> {
        self.coords_calls += 1;
        self.coords_results.pop_front().unwrap_or(None)
    }

    fn sign_many(&mut self, requests: &[SignReq]) -> Vec<CheckInResult> {
        self.sign_requests.push(requests.to_vec());
        if let Some(results) = self.sign_results.pop_front() {
            return results;
        }
        requests
            .iter()
            .map(|request| CheckInResult::Ok(format!("mock ok: {}", request.activity.id)))
            .collect()
    }

    fn arrival_count(&mut self, _activity_id: &str) -> Result<(u32, u32), ApiErr> {
        self.arrival_calls += 1;
        self.arrival_results
            .pop_front()
            .unwrap_or(Ok((100, 100)))
    }
}
