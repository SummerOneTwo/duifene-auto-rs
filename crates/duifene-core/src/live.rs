use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration as StdDuration;

use rand::Rng;
use ureq::http::HeaderMap;
use ureq::tls::{TlsConfig, TlsProvider};
use ureq::{Agent, Body};

use crate::api::{ApiErr, CheckInResult, Client, SignReq};
use crate::models::{Course, RawRow, SignActivity, SignType};

type HttpClient = Agent;
type Response = ureq::http::Response<Body>;

const HOST: &str = "https://www.duifene.com";
const DESKTOP_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/148.0.0.0 Safari/537.36 Edg/148.0.0.0";
const MOBILE_UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 16_6 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Mobile/15E148 MicroMessenger/8.0.40(0x1800282a) NetType/WIFI Language/zh_CN";
const FORM_CONTENT_TYPE: &str = "application/x-www-form-urlencoded; charset=UTF-8";
const LOCATION_JITTER: f64 = 0.000089;

fn parse_cookie_pair(text: &str) -> Option<(String, String)> {
    let first_segment = text.split(';').next()?.trim();
    let (name, value) = first_segment.split_once('=')?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), value.trim().to_string()))
}

#[derive(Clone)]
struct CookieStore(Arc<Mutex<CookieInner>>);

struct CookieInner {
    entries: Vec<(String, String)>,
    header: Option<String>,
}

impl CookieStore {
    fn new() -> Self {
        CookieStore(Arc::new(Mutex::new(CookieInner {
            entries: Vec::new(),
            header: None,
        })))
    }

    fn insert_pair(&self, name: String, value: String) {
        let mut inner = self.0.lock().unwrap();
        if let Some(existing) = inner.entries.iter_mut().find(|(key, _)| *key == name) {
            if existing.1 == value {
                return;
            }
            existing.1 = value;
        } else {
            inner.entries.push((name, value));
        }
        inner.header = None;
    }

    fn seed(&self, cookie_string: &str) {
        for segment in cookie_string.split(';') {
            if let Some((name, value)) = parse_cookie_pair(segment) {
                self.insert_pair(name, value);
            }
        }
    }

    fn clear(&self) {
        let mut inner = self.0.lock().unwrap();
        inner.entries.clear();
        inner.header = None;
    }

    fn apply_response(&self, headers: &HeaderMap) {
        for value in headers.get_all("set-cookie") {
            if let Ok(text) = value.to_str()
                && let Some((name, value)) = parse_cookie_pair(text)
            {
                self.insert_pair(name, value);
            }
        }
    }

    fn header_value(&self) -> Option<String> {
        let mut inner = self.0.lock().unwrap();
        if inner.entries.is_empty() {
            return None;
        }
        if inner.header.is_none() {
            let text = inner
                .entries
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect::<Vec<String>>()
                .join("; ");
            inner.header = Some(text);
        }
        Some(inner.header.clone().unwrap_or_default())
    }

    fn snapshot(&self) -> String {
        self.header_value().unwrap_or_default()
    }

    fn summary(&self) -> String {
        let inner = self.0.lock().unwrap();
        if inner.entries.is_empty() {
            return "无".to_string();
        }
        inner
            .entries
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<&str>>()
            .join(",")
    }
}

fn execute_request(
    http: &HttpClient,
    cookies: &CookieStore,
    path: &str,
    data: Option<&str>,
    referer: Option<&str>,
    extra_headers: &[(&str, &str)],
    mobile_ua: bool,
) -> Result<Response, ApiErr> {
    let mut target_url = format!("{HOST}{path}");
    let mut redirects = 0;
    let mut retried = false;
    loop {
        let is_first_hop = redirects == 0;
        let mut headers: Vec<(&str, String)> = Vec::new();
        if let Some(cookie_value) = cookies.header_value() {
            headers.push(("Cookie", cookie_value));
        }
        if mobile_ua {
            headers.push(("User-Agent", MOBILE_UA.to_string()));
        }
        if let Some(referer_value) = referer {
            headers.push(("Referer", referer_value.to_string()));
        }
        for (name, value) in extra_headers {
            headers.push((name, (*value).to_string()));
        }
        let response = if is_first_hop && let Some(body) = data {
            let mut builder = http.post(&target_url);
            for (name, value) in &headers {
                builder = builder.header(*name, value.as_str());
            }
            builder
                .header("Content-Type", FORM_CONTENT_TYPE)
                .send(body.to_string())
        } else {
            let mut builder = http.get(&target_url);
            for (name, value) in &headers {
                builder = builder.header(*name, value.as_str());
            }
            builder.call()
        }
        .map_err(|error| ApiErr::Network(error.to_string()))?;
        cookies.apply_response(response.headers());
        let status = response.status();
        if status.as_u16() == 429 && !retried {
            retried = true;
            thread::sleep(StdDuration::from_millis(500));
            continue;
        }
        if let Some(location) = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
        {
            redirects += 1;
            if redirects > 12 {
                return Err(ApiErr::Msg("重定向次数过多".to_string()));
            }
            target_url = if location.starts_with("http://") || location.starts_with("https://") {
                location.to_string()
            } else if location.starts_with("//") {
                format!("https:{location}")
            } else {
                format!("{HOST}{location}")
            };
            continue;
        }
        if !status.is_success() {
            return Err(ApiErr::Http(status.as_u16()));
        }
        return Ok(response);
    }
}

fn parse_json(mut response: Response) -> Result<serde_json::Value, ApiErr> {
    let text = response
        .body_mut()
        .read_to_string()
        .map_err(|error| ApiErr::Parse(error.to_string()))?;
    serde_json::from_str(&text).map_err(|error| ApiErr::Parse(error.to_string()))
}

fn parse_text(mut response: Response) -> Result<String, ApiErr> {
    response
        .body_mut()
        .read_to_string()
        .map_err(|error| ApiErr::Network(error.to_string()))
}

fn extract_value(text: &str, marker_position: usize) -> Option<String> {
    let after = &text[marker_position + "value=".len()..];
    let after = after.trim_start();
    let first = after.chars().next()?;
    if first == '"' || first == '\'' {
        let relative_closing = after[1..].find(first)?;
        return Some(after[1..1 + relative_closing].to_string());
    }
    let end = after
        .find(|character: char| character.is_whitespace() || character == '>')
        .unwrap_or(after.len());
    Some(after[..end].to_string())
}

fn find_value_forward(html: &str, from_position: usize, window: usize) -> Option<String> {
    let end = (from_position + window).min(html.len());
    let relative = html[from_position..end].find("value=")?;
    extract_value(html, from_position + relative)
}

fn find_value_backward(html: &str, before_position: usize, window: usize) -> Option<String> {
    let start = before_position.saturating_sub(window);
    let region = &html[start..before_position];
    let mut last_marker: Option<usize> = None;
    let mut search_from = 0;
    while let Some(relative) = region[search_from..].find("value=") {
        last_marker = Some(search_from + relative);
        search_from += relative + "value=".len();
    }
    let marker = last_marker?;
    extract_value(html, start + marker)
}

fn find_input_value(html: &str, ids: &[&str]) -> Option<String> {
    for id in ids {
        for quote in ['"', '\''] {
            let pattern = format!("id={quote}{id}{quote}");
            let mut search_from = 0;
            while let Some(relative) = html[search_from..].find(&pattern) {
                let marker = search_from + relative;
                let after_marker = marker + pattern.len();
                if let Some(value) = find_value_forward(html, after_marker, 300) {
                    return Some(value);
                }
                if let Some(value) = find_value_backward(html, marker, 300) {
                    return Some(value);
                }
                search_from = after_marker;
            }
        }
    }
    None
}

fn extract_login_name(html: &str) -> Option<String> {
    let marker = "id=\"aLoginName\"";
    let marker_pos = html.find(marker)?;
    let after = &html[marker_pos + marker.len()..];
    let open = after.find('>')?;
    let body = &after[open + 1..];
    let close = body.find("</a>").or_else(|| body.find("</A>"))?;
    let name = body[..close].trim().to_string();
    if name.is_empty() { None } else { Some(name) }
}

fn find_input_value_by_keyword(html: &str, keywords: &[&str]) -> Option<String> {
    let mut search_from = 0;
    while let Some(relative) = html[search_from..].find("name=") {
        let marker = search_from + relative;
        if let Some(name) = extract_value(html, marker) {
            let lower_name = name.to_lowercase();
            if keywords.iter().any(|keyword| lower_name.contains(keyword))
                && let Some(value) = find_value_forward(html, marker, 300)
            {
                return Some(value);
            }
        }
        search_from = marker + "name=".len();
    }
    None
}

fn page_coordinates(html: &str) -> Option<(f64, f64)> {
    let longitude_text = find_input_value(html, &["HFRoomLongitude", "hfroomlongitude"])?;
    let latitude_text = find_input_value(html, &["HFRoomLatitude", "hfroomlatitude"])?;
    match (longitude_text.parse::<f64>(), latitude_text.parse::<f64>()) {
        (Ok(longitude), Ok(latitude)) => Some((longitude, latitude)),
        _ => None,
    }
}

fn round_to_8(value: f64) -> f64 {
    (value * 1e8).round() / 1e8
}

fn jittered(longitude: f64, latitude: f64) -> (f64, f64) {
    let mut rng = rand::rng();
    let longitude = longitude + rng.random_range(-LOCATION_JITTER..LOCATION_JITTER);
    let latitude = latitude + rng.random_range(-LOCATION_JITTER..LOCATION_JITTER);
    (round_to_8(longitude), round_to_8(latitude))
}

const TRIANGLE_BASE_LONGITUDE: f64 = 108.0;
const TRIANGLE_BASE_LATITUDE: f64 = 34.0;
const TRIANGLE_SPREAD_DEGREES: f64 = 0.002;
const LATITUDE_METERS_PER_DEGREE: f64 = 110574.0;

fn longitude_meters_per_degree(latitude: f64) -> f64 {
    111320.0 * latitude.to_radians().cos()
}

fn location_body(
    uid: &str,
    class_id: &str,
    tclass_id: &str,
    longitude: f64,
    latitude: f64,
) -> String {
    format!(
        "action=signin&cid={class_id}&tcid={tclass_id}&sid={uid}&latitude={latitude:.8}&longitude={longitude:.8}"
    )
}

fn submit_location(
    http: &HttpClient,
    cookies: &CookieStore,
    uid: &str,
    class_id: &str,
    tclass_id: &str,
    longitude: f64,
    latitude: f64,
) -> CheckInResult {
    let data = location_body(uid, class_id, tclass_id, longitude, latitude);
    let response = execute_request(
        http,
        cookies,
        "/_CheckIn/CheckInRoomHandler.ashx",
        Some(&data),
        Some(&format!(
            "{HOST}/_CheckIn/MB/CheckInStudent.aspx?moduleid=16&pasd="
        )),
        &[("X-Requested-With", "XMLHttpRequest")],
        true,
    );
    match response {
        Ok(response) => classify_sign_json(response, "签到未成功"),
        Err(error) => CheckInResult::Failed(format!("签到异常: {error}")),
    }
}

fn parse_distance_meters(message: &str) -> Option<f64> {
    let marker = message.find("距离")?;
    let tail = &message[marker..];
    let start = tail.find(|character: char| character.is_ascii_digit())?;
    let mut end = start;
    while end < tail.len()
        && (tail.as_bytes()[end].is_ascii_digit() || tail.as_bytes()[end] == b'.')
    {
        end += 1;
    }
    tail[start..end].parse::<f64>().ok()
}

fn compute_center_from(
    base_longitude: f64,
    base_latitude: f64,
    d1: f64,
    d2: f64,
    d3: f64,
) -> (f64, f64) {
    let lng_scale = longitude_meters_per_degree(base_latitude);
    let x2 = TRIANGLE_SPREAD_DEGREES * lng_scale;
    let y3 = TRIANGLE_SPREAD_DEGREES * LATITUDE_METERS_PER_DEGREE;
    let x = (d1 * d1 - d2 * d2 + x2 * x2) / (2.0 * x2);
    let y = (d1 * d1 - d3 * d3 + y3 * y3) / (2.0 * y3);
    (
        base_longitude + x / lng_scale,
        base_latitude + y / LATITUDE_METERS_PER_DEGREE,
    )
}

fn triangulate_location(
    http: &HttpClient,
    cookies: &CookieStore,
    uid: &str,
    class_id: &str,
    tclass_id: &str,
    base: (f64, f64),
) -> (CheckInResult, Option<(f64, f64)>) {
    let mut base = base;
    let offsets = [
        (0.0, 0.0),
        (TRIANGLE_SPREAD_DEGREES, 0.0),
        (0.0, TRIANGLE_SPREAD_DEGREES),
    ];
    for _round in 0..3 {
        let mut distances = [0.0f64; 3];
        for (index, (longitude_offset, latitude_offset)) in offsets.into_iter().enumerate() {
            let probe_point = (base.0 + longitude_offset, base.1 + latitude_offset);
            let result = submit_location(
                http,
                cookies,
                uid,
                class_id,
                tclass_id,
                probe_point.0,
                probe_point.1,
            );
            match result {
                CheckInResult::Ok(message) => {
                    return (CheckInResult::Ok(message), Some(probe_point));
                }
                CheckInResult::Gone(message) => return (CheckInResult::Gone(message), None),
                CheckInResult::Failed(message) => match parse_distance_meters(&message) {
                    Some(distance) => distances[index] = distance,
                    None => {
                        return (
                            CheckInResult::Failed(format!("三角定位失败: {message}")),
                            None,
                        );
                    }
                },
            }
            thread::sleep(StdDuration::from_millis(300));
        }
        let center = compute_center_from(base.0, base.1, distances[0], distances[1], distances[2]);
        let (longitude, latitude) = jittered(center.0, center.1);
        match submit_location(http, cookies, uid, class_id, tclass_id, longitude, latitude) {
            CheckInResult::Ok(message) => {
                return (CheckInResult::Ok(message), Some(center));
            }
            CheckInResult::Gone(message) => return (CheckInResult::Gone(message), None),
            CheckInResult::Failed(message) => match parse_distance_meters(&message) {
                Some(distance) if distance < 2.0 => return (CheckInResult::Failed(message), None),
                Some(_) => base = center,
                None => {
                    return (
                        CheckInResult::Failed(format!("三角定位失败: {message}")),
                        None,
                    );
                }
            },
        }
    }
    (
        CheckInResult::Failed("多次三角定位未成功".to_string()),
        None,
    )
}

fn classify_message(message: &str, failure_prefix: &str) -> CheckInResult {
    if message.contains("成功") {
        return CheckInResult::Ok(message.to_string());
    }
    if message.contains("已结束") || message.contains("没有正在") {
        return CheckInResult::Gone(message.to_string());
    }
    CheckInResult::Failed(format!("{failure_prefix}: {message}"))
}

fn classify_sign_json(response: Response, failure_prefix: &str) -> CheckInResult {
    match parse_json(response) {
        Ok(json) => {
            let message = json
                .get("msgbox")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if message.is_empty() {
                CheckInResult::Failed(failure_prefix.to_string())
            } else {
                classify_message(&message, failure_prefix)
            }
        }
        Err(error) => CheckInResult::Failed(format!("{failure_prefix}: {error}")),
    }
}

fn first_text_snippet(html: &str) -> String {
    let collapsed = html.split_whitespace().collect::<Vec<&str>>().join(" ");
    collapsed.chars().take(60).collect()
}

fn parse_courses(items: &[serde_json::Value]) -> Vec<Course> {
    let mut courses = Vec::new();
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let string_field = |key: &str| -> String {
            object
                .get(key)
                .and_then(|value| match value {
                    serde_json::Value::String(text) => Some(text.clone()),
                    serde_json::Value::Number(number) => Some(number.to_string()),
                    _ => None,
                })
                .unwrap_or_default()
        };
        let course = Course {
            class_id: string_field("CourseID"),
            tclass_id: string_field("TClassID"),
            name: string_field("CourseName"),
        };
        if course.tclass_id.is_empty() && course.name.is_empty() {
            continue;
        }
        let term = course_term(&string_field("TermName"));
        courses.push((course, term));
    }

    let latest = courses
        .iter()
        .filter_map(|(_, term)| *term)
        .max_by_key(|term| (term.year, term.is_autumn));
    let latest_key = latest.map(|term| (term.year, term.is_autumn));
    courses
        .into_iter()
        .filter_map(|(course, term)| match term {
            None => Some(course),
            Some(term) if Some((term.year, term.is_autumn)) == latest_key => Some(course),
            _ => None,
        })
        .collect()
}

#[derive(Clone, Copy)]
struct CourseTerm {
    year: i32,
    is_autumn: bool,
}

fn course_term(term_name: &str) -> Option<CourseTerm> {
    let (year_text, season) = term_name.split_once("年")?;
    let year = year_text.trim().parse::<i32>().ok()?;
    match season.trim() {
        "秋" => Some(CourseTerm {
            year,
            is_autumn: true,
        }),
        "春" => Some(CourseTerm {
            year,
            is_autumn: false,
        }),
        _ => None,
    }
}

fn rows_from_json(json: &serde_json::Value) -> Vec<RawRow> {
    let mut rows = Vec::new();
    let Some(array) = json.get("rows").and_then(|value| value.as_array()) else {
        return rows;
    };
    for item in array {
        if let Ok(raw) = serde_json::from_value::<RawRow>(item.clone()) {
            rows.push(raw);
        }
    }
    rows
}

pub struct LiveClient {
    http: HttpClient,
    cookies: CookieStore,
    uid: Option<String>,
    discovered: HashSet<String>,
    learned_center: Option<(f64, f64)>,
}

impl LiveClient {
    pub fn new() -> Self {
        let tls = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .disable_verification(true)
            .build();
        let http = Agent::config_builder()
            .tls_config(tls)
            .max_redirects(0)
            .http_status_as_error(false)
            .timeout_global(Some(StdDuration::from_secs(10)))
            .timeout_connect(Some(StdDuration::from_secs(5)))
            .max_idle_connections_per_host(16)
            .user_agent(DESKTOP_UA)
            .build()
            .new_agent();
        LiveClient {
            http,
            cookies: CookieStore::new(),
            uid: None,
            discovered: HashSet::new(),
            learned_center: crate::storage::load_learned_center(),
        }
    }

    pub fn from_cookie_string(cookie_string: &str) -> Self {
        let client = LiveClient::new();
        client.cookies.seed(cookie_string);
        client
    }

    pub fn cookie_string(&self) -> String {
        self.cookies.snapshot()
    }

    fn get(&self, path: &str) -> Result<Response, ApiErr> {
        execute_request(&self.http, &self.cookies, path, None, None, &[], false)
    }

    fn post_with_headers(
        &self,
        path: &str,
        data: String,
        referer_path: &str,
        extra_headers: &[(&str, &str)],
    ) -> Result<Response, ApiErr> {
        execute_request(
            &self.http,
            &self.cookies,
            path,
            Some(&data),
            Some(&format!("{HOST}{referer_path}")),
            extra_headers,
            false,
        )
    }

    pub fn login_wechat(&mut self, authorization_link: &str) -> Result<(), ApiErr> {
        let code = extract_wechat_code(authorization_link)
            .ok_or_else(|| ApiErr::Parse("无法提取授权码".to_string()))?;
        self.cookies.clear();
        let path = format!("/P.aspx?authtype=1&code={code}&state=1");
        let exchange_page = parse_text(self.get(&path)?)
            .map(|html| first_text_snippet(&html))
            .unwrap_or_default();
        let cookie_summary = self.cookies.summary();
        match self.verify_session("微信登录后会话未生效") {
            Ok(()) => Ok(()),
            Err(ApiErr::Msg(message)) => Err(ApiErr::Msg(format!(
                "{message}; 交换后 cookie: {cookie_summary}; 交换页: {exchange_page}"
            ))),
            Err(error) => Err(error),
        }
    }

    fn verify_session(&self, failure_prefix: &str) -> Result<(), ApiErr> {
        let response = self.post_with_headers(
            "/AppCode/LoginInfo.ashx",
            "Action=checklogin".to_string(),
            "/_UserCenter/PC/CenterStudent.aspx",
            &[],
        )?;
        let json = parse_json(response)?;
        match json.get("msg").and_then(|value| value.as_str()) {
            Some("1") => Ok(()),
            _ => {
                let message = json
                    .get("msgbox")
                    .and_then(|value| value.as_str())
                    .unwrap_or("未知原因");
                Err(ApiErr::Msg(format!("{failure_prefix}: {message}")))
            }
        }
    }

    fn uid(&mut self) -> Result<String, ApiErr> {
        if let Some(cached) = &self.uid {
            return Ok(cached.clone());
        }
        let response = self.get("/_UserCenter/MB/index.aspx")?;
        let html = parse_text(response)?;
        let found = find_input_value(
            &html,
            &["hidUID", "hidUid", "hidUserId", "hidSID", "studentId"],
        )
        .or_else(|| find_input_value_by_keyword(&html, &["uid", "userid", "studentid"]));
        match found {
            Some(uid) if !uid.is_empty() => {
                self.uid = Some(uid.clone());
                Ok(uid)
            }
            _ => Err(ApiErr::Parse("无法获取学生ID".to_string())),
        }
    }

    fn ensure_discovered(&mut self, course: &Course) -> Result<(), ApiErr> {
        if self.discovered.contains(&course.tclass_id) {
            return Ok(());
        }
        let page_path = format!(
            "/_CheckIn/PC/StudentNoCheckCount.aspx?classid={}",
            course.tclass_id
        );
        let page_html = parse_text(self.get(&page_path)?)?;
        let page_uid = find_input_value(
            &page_html,
            &[
                "hidUID",
                "hidUid",
                "hidUserId",
                "hidSID",
                "studentId",
                "HidStudentID",
            ],
        );
        let uid_string = match page_uid {
            Some(uid) if !uid.is_empty() => {
                if self.uid.is_none() {
                    self.uid = Some(uid.clone());
                }
                uid
            }
            _ => self.uid()?,
        };
        let data = format!(
            "action=getstudentinlogbyday&classid={}&studentid={uid_string}",
            course.tclass_id
        );
        let response = self.post_with_headers(
            "/_CheckIn/MBCount.ashx",
            data,
            "/_CheckIn/PC/StudentNoCheckCount.aspx",
            &[
                ("X-Requested-With", "XMLHttpRequest"),
                ("Origin", HOST),
                ("Accept", "application/json, text/javascript, */*; q=0.01"),
            ],
        )?;
        let json = parse_json(response)?;
        if json.get("rows").is_some() {
            self.discovered.insert(course.tclass_id.clone());
            Ok(())
        } else {
            Err(ApiErr::Parse("签到接口探测失败".to_string()))
        }
    }

    pub fn login_name(&mut self) -> Result<String, ApiErr> {
        let response = self.get("/_UserCenter/PC/PersonalDetials.aspx")?;
        let html = parse_text(response)?;
        extract_login_name(&html).ok_or_else(|| ApiErr::Parse("无法获取登录名".to_string()))
    }

    fn enter_course(&self, course: &Course) -> Result<(), ApiErr> {
        let path = format!("/_UserCenter/MB/Module.aspx?data={}", course.class_id);
        let response = execute_request(
            &self.http,
            &self.cookies,
            &path,
            None,
            Some(&format!("{HOST}/_UserCenter/MB/index.aspx")),
            &[],
            false,
        )?;
        let html = parse_text(response)?;
        if html.contains(&course.class_id) {
            Ok(())
        } else {
            Err(ApiErr::Msg("进入课程页面失败".to_string()))
        }
    }
}

impl Default for LiveClient {
    fn default() -> Self {
        LiveClient::new()
    }
}

fn extract_wechat_code(link: &str) -> Option<String> {
    let marker = link.find("code=")?;
    let rest = &link[marker + "code=".len()..];
    let code: String = rest
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect();
    if code.len() == 32 { Some(code) } else { None }
}

impl Client for LiveClient {
    fn check_login(&mut self) -> Result<(), ApiErr> {
        self.verify_session("登录已失效")
    }

    fn fetch_courses(&mut self) -> Result<Vec<Course>, ApiErr> {
        let response = self.post_with_headers(
            "/_UserCenter/CourseInfo.ashx",
            "action=getstudentcourse&classtypeid=2".to_string(),
            "/_UserCenter/PC/CenterStudent.aspx",
            &[],
        )?;
        let json = parse_json(response)?;
        let list_value = if let Some(array) = json.as_array() {
            array
        } else {
            let mut found = None;
            for key in ["data", "list", "courses", "items"] {
                if let Some(array) = json.get(key).and_then(|value| value.as_array()) {
                    found = Some(array);
                    break;
                }
            }
            found.ok_or_else(|| ApiErr::Parse("课程列表结构不符".to_string()))?
        };
        Ok(parse_courses(list_value))
    }

    fn fetch_all(&mut self, courses: &[Course]) -> Vec<Result<Vec<RawRow>, ApiErr>> {
        let uid_string = match self.uid() {
            Ok(uid) => uid,
            Err(_) => {
                let error = ApiErr::Msg("无法获取用户ID".to_string());
                return vec![Err(error); courses.len()];
            }
        };
        let mut outcomes: Vec<Option<Result<Vec<RawRow>, ApiErr>>> = vec![None; courses.len()];
        for (index, course) in courses.iter().enumerate() {
            if let Err(error) = self.ensure_discovered(course) {
                outcomes[index] = Some(Err(error));
            }
        }
        let http = self.http.clone();
        let cookies = self.cookies.clone();
        thread::scope(|scope| {
            let mut handles = Vec::new();
            for (index, course) in courses.iter().enumerate() {
                if outcomes[index].is_some() {
                    continue;
                }
                let http = http.clone();
                let cookies = cookies.clone();
                let course = course.clone();
                let uid_string = uid_string.clone();
                let handle = scope.spawn(move || {
                    let data = format!(
                        "action=getstudentinlogbyday&classid={}&studentid={uid_string}",
                        course.tclass_id
                    );
                    let response = execute_request(
                        &http,
                        &cookies,
                        "/_CheckIn/MBCount.ashx",
                        Some(&data),
                        Some(&format!("{HOST}/_UserCenter/MB/index.aspx")),
                        &[],
                        false,
                    )?;
                    let json = parse_json(response)?;
                    if json.get("msg").and_then(|value| value.as_str()) != Some("1") {
                        return Ok(Vec::new());
                    }
                    Ok(rows_from_json(&json))
                });
                handles.push((index, handle));
            }
            for (index, handle) in handles {
                outcomes[index] = Some(
                    handle
                        .join()
                        .unwrap_or(Err(ApiErr::Msg("拉取线程异常".to_string()))),
                );
            }
        });
        outcomes
            .into_iter()
            .map(|outcome| outcome.unwrap_or(Ok(Vec::new())))
            .collect()
    }

    fn activity_coords(&mut self, activity: &SignActivity, course: &Course) -> Option<(f64, f64)> {
        if activity.coordinate.is_some() {
            return activity.coordinate;
        }
        if activity.r#type != SignType::Locate {
            return None;
        }
        if self.enter_course(course).is_err() {
            return None;
        }
        let current_activity_page = format!(
            "/_CheckIn/MB/TeachCheckIn.aspx?classid={}&temps=0&checktype=1&isrefresh=0&timeinterval=0&roomid=0&match=",
            course.tclass_id
        );
        let fallback_page = format!(
            "/_CheckIn/MB/TeachCheckIn.aspx?classid={}&temps=0&checktype=3&isrefresh=0&timeinterval=0&roomid=0&match=",
            course.tclass_id
        );
        for path in [current_activity_page, fallback_page] {
            let Ok(response) = self.get(&path) else {
                continue;
            };
            let Ok(html) = parse_text(response) else {
                continue;
            };
            if std::env::var_os("DUIFENE_DEBUG").is_some() {
                let page_id = find_input_value(&html, &["HFCheckInID"]).unwrap_or_default();
                let page_type = find_input_value(&html, &["HFChecktype"]).unwrap_or_default();
                eprintln!(
                    "[debug] coords page={path} page activity id={page_id}, wanted={}, type={page_type}",
                    activity.id
                );
            }
            if let Some(page_activity_id) = find_input_value(&html, &["HFCheckInID"])
                && page_activity_id != activity.id
            {
                continue;
            }
            if let Some(coordinates) = page_coordinates(&html) {
                if std::env::var_os("DUIFENE_DEBUG").is_some() {
                    eprintln!("[debug] coords page hit: {coordinates:?}");
                }
                return Some(coordinates);
            }
        }
        None
    }

    fn sign_many(&mut self, requests: &[SignReq]) -> Vec<CheckInResult> {
        let uid_string = match self.uid() {
            Ok(uid) => uid,
            Err(_) => {
                return requests
                    .iter()
                    .map(|_| CheckInResult::Failed("无法获取用户ID".to_string()))
                    .collect();
            }
        };

        enum SignAction {
            Post {
                path: &'static str,
                data: String,
                mobile_ua: bool,
            },
            Get {
                path: String,
            },
        }

        let mut pre_results: Vec<Option<CheckInResult>> = Vec::with_capacity(requests.len());
        let mut actions: Vec<Option<SignAction>> = Vec::with_capacity(requests.len());
        let mut located_indexes: Vec<(usize, (f64, f64))> = Vec::new();
        for (request_index, request) in requests.iter().enumerate() {
            let action = match request.activity.r#type {
                SignType::Code => match &request.activity.code {
                    Some(code) => Some(SignAction::Post {
                        path: "/_CheckIn/CheckIn.ashx",
                        data: format!(
                            "action=studentcheckin&studentid={uid_string}&checkincode={code}"
                        ),
                        mobile_ua: false,
                    }),
                    None => {
                        pre_results.push(Some(CheckInResult::Failed("未获取到签到码".to_string())));
                        actions.push(None);
                        continue;
                    }
                },
                SignType::Qr => Some(SignAction::Get {
                    path: format!(
                        "/_CheckIn/MB/QrCodeCheckOK.aspx?state={}",
                        request.activity.id
                    ),
                }),
                SignType::Locate => match request.coords {
                    Some((longitude, latitude)) => {
                        located_indexes.push((request_index, (longitude, latitude)));
                        let (longitude, latitude) = jittered(longitude, latitude);
                        Some(SignAction::Post {
                            path: "/_CheckIn/CheckInRoomHandler.ashx",
                            data: location_body(
                                &uid_string,
                                &request.course.class_id,
                                &request.course.tclass_id,
                                longitude,
                                latitude,
                            ),
                            mobile_ua: true,
                        })
                    }
                    None => {
                        let base = self
                            .learned_center
                            .unwrap_or((TRIANGLE_BASE_LONGITUDE, TRIANGLE_BASE_LATITUDE));
                        let (result, center) = triangulate_location(
                            &self.http,
                            &self.cookies,
                            &uid_string,
                            &request.course.class_id,
                            &request.course.tclass_id,
                            base,
                        );
                        if let Some(center) = center {
                            self.learned_center = Some(center);
                            let _ = crate::storage::save_learned_center(center);
                        }
                        pre_results.push(Some(result));
                        actions.push(None);
                        continue;
                    }
                },
                SignType::Unknown => {
                    pre_results.push(Some(CheckInResult::Failed("未知签到类型".to_string())));
                    actions.push(None);
                    continue;
                }
            };
            pre_results.push(None);
            actions.push(action);
        }

        let http = self.http.clone();
        let cookies = self.cookies.clone();
        let mut results: Vec<Option<CheckInResult>> = pre_results;
        thread::scope(|scope| {
            let mut handles = Vec::new();
            for (index, action) in actions.into_iter().enumerate() {
                let Some(action) = action else {
                    continue;
                };
                let http = http.clone();
                let cookies = cookies.clone();
                let handle = scope.spawn(move || match action {
                    SignAction::Post {
                        path,
                        data,
                        mobile_ua,
                    } => {
                        let extra_headers: &[(&str, &str)] = if mobile_ua {
                            &[("X-Requested-With", "XMLHttpRequest")]
                        } else {
                            &[]
                        };
                        let response = execute_request(
                            &http,
                            &cookies,
                            path,
                            Some(&data),
                            Some(&format!(
                                "{HOST}/_CheckIn/MB/CheckInStudent.aspx?moduleid=16&pasd="
                            )),
                            extra_headers,
                            mobile_ua,
                        );
                        match response {
                            Ok(response) => classify_sign_json(response, "签到未成功"),
                            Err(error) => CheckInResult::Failed(format!("签到异常: {error}")),
                        }
                    }
                    SignAction::Get { path } => {
                        let response =
                            execute_request(&http, &cookies, &path, None, None, &[], false);
                        match response {
                            Ok(response) => match parse_text(response) {
                                Ok(html) => {
                                    if html.contains("签到成功") {
                                        CheckInResult::Ok("签到成功".to_string())
                                    } else if html.contains("非微信") {
                                        CheckInResult::Failed(
                                            "非微信链接登录,无法二维码签到".to_string(),
                                        )
                                    } else {
                                        CheckInResult::Failed(format!(
                                            "二维码签到未成功: {}",
                                            first_text_snippet(&html)
                                        ))
                                    }
                                }
                                Err(error) => {
                                    CheckInResult::Failed(format!("二维码签到异常: {error}"))
                                }
                            },
                            Err(error) => CheckInResult::Failed(format!("二维码签到异常: {error}")),
                        }
                    }
                });
                handles.push((index, handle));
            }
            for (index, handle) in handles {
                results[index] = Some(
                    handle
                        .join()
                        .unwrap_or(CheckInResult::Failed("签到线程异常".to_string())),
                );
            }
        });
        for (request_index, center) in located_indexes {
            if matches!(results[request_index], Some(CheckInResult::Ok(_))) {
                self.learned_center = Some(center);
                let _ = crate::storage::save_learned_center(center);
            }
        }
        results
            .into_iter()
            .map(|result| result.unwrap_or(CheckInResult::Failed("未知错误".to_string())))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uid_parsed_with_double_quotes() {
        let html = r#"<input type="hidden" name="hidUID" id="hidUID" value="S202301" />"#;
        assert_eq!(
            find_input_value(html, &["hidUID", "studentId"]).as_deref(),
            Some("S202301")
        );
    }

    #[test]
    fn uid_parsed_with_single_quotes() {
        let html = r#"<input type='hidden' id='studentId' value='S202302' />"#;
        assert_eq!(
            find_input_value(html, &["hidUID", "studentId"]).as_deref(),
            Some("S202302")
        );
    }

    #[test]
    fn uid_parsed_when_value_precedes_id() {
        let html = r#"<input type="hidden" value="S202303" id="hidUID" />"#;
        assert_eq!(
            find_input_value(html, &["hidUID"]).as_deref(),
            Some("S202303")
        );
    }

    #[test]
    fn uid_parsed_by_name_keyword_fallback() {
        let html = r#"<input type="hidden" name="studentid_extra" value="S202304" />"#;
        assert_eq!(
            find_input_value_by_keyword(html, &["uid", "userid", "studentid"]).as_deref(),
            Some("S202304")
        );
    }

    #[test]
    fn room_coordinates_parsed_from_page() {
        let html = r#"<input id="HFRoomLongitude" value="114.39437" />
                      <input id="hfroomlatitude" value="22.70462" />"#;
        assert_eq!(page_coordinates(html), Some((114.39437, 22.70462)));
    }

    #[test]
    fn sign_message_classification() {
        assert!(matches!(
            classify_message("签到成功", "签到未成功"),
            CheckInResult::Ok(_)
        ));
        assert!(matches!(
            classify_message("活动已结束", "签到未成功"),
            CheckInResult::Gone(_)
        ));
        assert!(matches!(
            classify_message("没有正在进行的签到", "签到未成功"),
            CheckInResult::Gone(_)
        ));
        assert!(matches!(
            classify_message("已超出签到范围", "签到未成功"),
            CheckInResult::Failed(_)
        ));
    }

    #[test]
    fn cookie_pairs_parsed() {
        assert_eq!(
            parse_cookie_pair("session=abc123; Path=/; HttpOnly"),
            Some(("session".to_string(), "abc123".to_string()))
        );
        let store = CookieStore::new();
        store.seed("a=1; b=2");
        assert_eq!(store.header_value().as_deref(), Some("a=1; b=2"));
    }

    #[test]
    fn wechat_code_extracted() {
        let link = "https://example.com/auth?code=ABCDEF0123456789abcdef0123456789&state=1";
        assert_eq!(
            extract_wechat_code(link).as_deref(),
            Some("ABCDEF0123456789abcdef0123456789")
        );
        assert_eq!(extract_wechat_code("no code here"), None);
    }

    #[test]
    fn jitter_stays_within_bounds() {
        for _ in 0..50 {
            let (longitude, latitude) = jittered(114.39437, 22.70462);
            assert!((longitude - 114.39437).abs() <= LOCATION_JITTER + 1e-9);
            assert!((latitude - 22.70462).abs() <= LOCATION_JITTER + 1e-9);
        }
    }

    #[test]
    fn course_list_accepts_strings_numbers_and_junk() {
        let json = serde_json::json!([
            {"CourseID": "101", "TClassID": "c1", "CourseName": "软件工程"},
            {"CourseID": 102, "TClassID": "c2", "CourseName": "数据结构"},
            "垃圾行",
            {"CourseID": "103"},
        ]);
        let courses = parse_courses(json.as_array().unwrap());
        assert_eq!(courses.len(), 2);
        assert_eq!(courses[0].class_id, "101");
        assert_eq!(courses[0].name, "软件工程");
        assert_eq!(courses[1].class_id, "102");
        assert_eq!(courses[1].tclass_id, "c2");
    }

    #[test]
    fn course_list_keeps_only_latest_term() {
        let json = serde_json::json!([
            {"CourseID": "1", "TClassID": "c1", "CourseName": "课A", "TermName": "2026年秋"},
            {"CourseID": "2", "TClassID": "c2", "CourseName": "课B", "TermName": "2026年秋"},
            {"CourseID": "3", "TClassID": "c3", "CourseName": "课C", "TermName": "2026年春"},
            {"CourseID": "4", "TClassID": "c4", "CourseName": "课D", "TermName": "2025年秋"},
        ]);
        let courses = parse_courses(json.as_array().unwrap());
        let names: Vec<&str> = courses.iter().map(|course| course.name.as_str()).collect();
        assert_eq!(names, ["课A", "课B"]);
    }

    #[test]
    fn course_list_keeps_long_term_courses() {
        let json = serde_json::json!([
            {"CourseID": "1", "TClassID": "c1", "CourseName": "课A", "TermName": "2026年秋"},
            {"CourseID": "2", "TClassID": "c2", "CourseName": "长期课", "TermName": "2024级形势与政策"},
            {"CourseID": "3", "TClassID": "c3", "CourseName": "课C", "TermName": "2025年秋"},
        ]);
        let courses = parse_courses(json.as_array().unwrap());
        let names: Vec<&str> = courses.iter().map(|course| course.name.as_str()).collect();
        assert_eq!(names, ["课A", "长期课"]);
    }

    #[test]
    fn course_list_autumn_beats_spring_of_same_year() {
        let json = serde_json::json!([
            {"CourseID": "1", "TClassID": "c1", "CourseName": "春课", "TermName": "2026年春"},
            {"CourseID": "2", "TClassID": "c2", "CourseName": "秋课", "TermName": "2026年秋"},
        ]);
        let courses = parse_courses(json.as_array().unwrap());
        let names: Vec<&str> = courses.iter().map(|course| course.name.as_str()).collect();
        assert_eq!(names, ["秋课"]);
    }

    #[test]
    fn course_list_without_term_keeps_everything() {
        let json = serde_json::json!([
            {"CourseID": "1", "TClassID": "c1", "CourseName": "甲"},
            {"CourseID": "2", "TClassID": "c2", "CourseName": "乙"},
        ]);
        let courses = parse_courses(json.as_array().unwrap());
        assert_eq!(courses.len(), 2);
    }

    #[test]
    fn login_name_extracted_from_details_page() {
        let html = r#"<td><a id="aLoginName" title="点击更改" onclick="LayerUpdate(this,20,&#39;修改登录名&#39;)">linyisu1024</a></td>"#;
        assert_eq!(extract_login_name(html).as_deref(), Some("linyisu1024"));
        let empty = r#"<td><a id="aLoginName"></a></td>"#;
        assert_eq!(extract_login_name(empty), None);
    }

    #[test]
    fn distance_parsed_from_rejection_message() {
        assert_eq!(
            parse_distance_meters("签到未成功: 不在教室范围，距离：343.08米！"),
            Some(343.08)
        );
        assert_eq!(
            parse_distance_meters("不在教室范围,距离: 557.68米"),
            Some(557.68)
        );
        assert_eq!(parse_distance_meters("签到成功"), None);
    }

    #[test]
    fn center_computed_from_real_measurements() {
        let (longitude, latitude) =
            compute_center_from(114.39437, 22.70462, 343.08, 350.48, 557.68);
        assert!((longitude - 114.39524833).abs() < 0.0001);
        assert!((latitude - 22.70166749).abs() < 0.0001);
    }
}
