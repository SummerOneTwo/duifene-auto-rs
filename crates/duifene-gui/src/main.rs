#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

slint::include_modules!();

use slint::winit_030::WinitWindowAccessor;

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use chrono::Local;

use duifene_core::api::{CheckInResult, Client as _};
use duifene_core::engine::{Engine, EngineConfig, Event as CoreEvent};
use duifene_core::live::LiveClient;
use duifene_core::runner::Runner;
use duifene_core::{models as core_models, storage};

const WECHAT_AUTH_URL: &str = "https://open.weixin.qq.com/connect/oauth2/authorize?appid=wx1b5650884f657981&redirect_uri=https://www.duifene.com/_FileManage/PdfView.aspx?file=https%3A%2F%2Ffs.duifene.com%2Fres%2Fr2%2Fu6106199%2F%E5%AF%B9%E5%88%86%E6%98%93%E7%99%BB%E5%BD%95_876c9d439ca68ead389c.pdf&response_type=code&scope=snsapi_userinfo&connect_redirect=1#wechat_redirect";

const MAX_EVENTS: usize = 200;
const MAX_ACTIVITIES: usize = 10;

#[derive(Clone, Copy, PartialEq, Eq)]
enum EventKind {
    Ok,
    Warn,
    Info,
}

struct EventLine {
    time: String,
    kind: EventKind,
    course: String,
    text: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ActivityStatus {
    Detected,
    Success,
    Failed,
    Gone,
}

struct ActivityData {
    id: String,
    course: String,
    kind: String,
    time: String,
    detail: String,
    status: ActivityStatus,
}

enum GuiMessage {
    Core(CoreEvent),
    Courses(Vec<core_models::Course>),
    MonitorStopped(String),
    SessionCheck(bool),
    LoginName(String),
    LoginOk,
    LoginFail(String),
}

/// UI 可变状态的权威数据;后台线程只通过消息泵投递,所有变更都在 UI 线程发生。
struct UiState {
    events: Vec<EventLine>,
    activities: Vec<ActivityData>,
}

struct Shared {
    state: Mutex<UiState>,
    stop_flag: Mutex<Option<Arc<AtomicBool>>>,
    started: Mutex<Option<Instant>>,
}

impl Shared {
    fn new() -> Self {
        Shared {
            state: Mutex::new(UiState {
                events: Vec::new(),
                activities: Vec::new(),
            }),
            stop_flag: Mutex::new(None),
            started: Mutex::new(None),
        }
    }
}

fn kind_value(kind: EventKind) -> i32 {
    match kind {
        EventKind::Ok => 0,
        EventKind::Warn => 1,
        EventKind::Info => 2,
    }
}

fn status_value(status: ActivityStatus) -> i32 {
    match status {
        ActivityStatus::Detected => 0,
        ActivityStatus::Success => 1,
        ActivityStatus::Failed => 2,
        ActivityStatus::Gone => 3,
    }
}

fn event_rows(state: &UiState) -> Vec<EventRow> {
    state
        .events
        .iter()
        .map(|line| EventRow {
            time: line.time.clone().into(),
            kind: kind_value(line.kind),
            course: line.course.clone().into(),
            text: line.text.clone().into(),
        })
        .collect()
}

fn activity_rows(state: &UiState) -> Vec<ActivityRow> {
    state
        .activities
        .iter()
        .map(|row| ActivityRow {
            course: row.course.clone().into(),
            kind: row.kind.clone().into(),
            time: row.time.clone().into(),
            detail: row.detail.clone().into(),
            status: status_value(row.status),
        })
        .collect()
}

fn normalized(rows: Vec<(String, u32, i32)>) -> Vec<StatRow> {
    let max = rows
        .iter()
        .map(|(_, count, _)| *count)
        .max()
        .unwrap_or(1)
        .max(1);
    rows.into_iter()
        .map(|(label, count, color)| StatRow {
            label: label.into(),
            count: count as i32,
            ratio: count as f32 / max as f32,
            color,
        })
        .collect()
}

fn stat_rows(activities: &[ActivityData]) -> (Vec<StatRow>, Vec<StatRow>, Vec<StatRow>) {
    let mut kind_counts: BTreeMap<String, (u32, i32)> = BTreeMap::new();
    let mut result_counts: BTreeMap<String, (u32, i32)> = BTreeMap::new();
    let mut course_counts: BTreeMap<String, u32> = BTreeMap::new();
    for activity in activities {
        *course_counts.entry(activity.course.clone()).or_insert(0) += 1;
        let kind = if activity.kind.is_empty() {
            "未知".to_string()
        } else {
            activity.kind.clone()
        };
        let color = match activity.kind.as_str() {
            "数字码" => 0,
            "二维码" => 1,
            "定位" => 2,
            _ => 4,
        };
        kind_counts.entry(kind).or_insert((0, color)).0 += 1;
        let result = match activity.status {
            ActivityStatus::Success => Some(("成功", 1)),
            ActivityStatus::Failed | ActivityStatus::Gone => Some(("失败", 3)),
            ActivityStatus::Detected => None,
        };
        if let Some((result, color)) = result {
            result_counts
                .entry(result.to_string())
                .or_insert((0, color))
                .0 += 1;
        }
    }
    let kind = normalized(
        kind_counts
            .into_iter()
            .map(|(label, (count, color))| (label, count, color))
            .collect(),
    );
    let result = normalized(
        result_counts
            .into_iter()
            .map(|(label, (count, color))| (label, count, color))
            .collect(),
    );
    let course_max = course_counts.values().copied().max().unwrap_or(1).max(1);
    const COURSE_COLORS: [i32; 5] = [0, 1, 2, 3, 4];
    let course = course_counts
        .into_iter()
        .enumerate()
        .map(|(index, (label, count))| StatRow {
            label: label.into(),
            count: count as i32,
            ratio: count as f32 / course_max as f32,
            color: COURSE_COLORS[index % COURSE_COLORS.len()],
        })
        .collect();
    (kind, result, course)
}

fn refresh_activity_views(logic: &Logic, shared: &Shared) {
    let (activities, (kind, result, course)) = {
        let state = shared.state.lock().unwrap();
        (activity_rows(&state), stat_rows(&state.activities))
    };
    logic.set_activities(Rc::new(slint::VecModel::from(activities)).into());
    logic.set_kind_stats(Rc::new(slint::VecModel::from(kind)).into());
    logic.set_result_stats(Rc::new(slint::VecModel::from(result)).into());
    logic.set_course_stats(Rc::new(slint::VecModel::from(course)).into());
}

fn push_event(logic: &Logic, shared: &Shared, kind: EventKind, course: &str, text: String) {
    let mut state = shared.state.lock().unwrap();
    state.events.insert(
        0,
        EventLine {
            time: Local::now().format("%H:%M:%S").to_string(),
            kind,
            course: course.to_string(),
            text,
        },
    );
    if state.events.len() > MAX_EVENTS {
        state.events.truncate(MAX_EVENTS);
    }
    logic.set_events(Rc::new(slint::VecModel::from(event_rows(&state))).into());
}

fn stop_monitor_ui(logic: &Logic, shared: &Shared) {
    if let Some(flag) = shared.stop_flag.lock().unwrap().as_ref() {
        flag.store(true, Ordering::Relaxed);
    }
    logic.set_monitoring(false);
    logic.set_elapsed("00:00:00".into());
    logic.set_courses_count(0);
}

fn spawn_monitor(tx: mpsc::Sender<GuiMessage>, flag: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let stopped = |reason: &str| {
            let _ = tx.send(GuiMessage::MonitorStopped(reason.to_string()));
        };
        let config = storage::load_config();
        if config.cookie.is_empty() {
            stopped("未登录");
            return;
        }
        let mut client = LiveClient::from_cookie_string(&config.cookie);
        if let Err(error) = client.check_login() {
            stopped(&format!("登录状态失效: {error}"));
            return;
        }
        let courses = match client.fetch_courses() {
            Ok(courses) => courses,
            Err(error) => {
                stopped(&format!("获取课程失败: {error}"));
                return;
            }
        };
        if flag.load(Ordering::Relaxed) {
            return;
        }
        let _ = tx.send(GuiMessage::Courses(courses.clone()));
        let engine = Engine::new(
            Box::new(client),
            courses,
            EngineConfig {
                delay_seconds: 0,
                coords: storage::course_coordinates(&config),
                refresh_every: 300,
            },
        );
        let mut runner = Runner::new(engine, Duration::from_secs(2));
        runner.run_with_stop(
            &mut |event| {
                let _ = tx.send(GuiMessage::Core(event));
            },
            &flag,
        );
        if !flag.load(Ordering::Relaxed) {
            stopped("监控任务已结束,请检查网络或登录状态");
        }
    });
}

fn spawn_fetch_courses(tx: mpsc::Sender<GuiMessage>) {
    std::thread::spawn(move || {
        let config = storage::load_config();
        let mut client = LiveClient::from_cookie_string(&config.cookie);
        if let Ok(name) = client.login_name() {
            let _ = tx.send(GuiMessage::LoginName(name));
        }
        match client.fetch_courses() {
            Ok(courses) => {
                let _ = tx.send(GuiMessage::Courses(courses));
            }
            Err(error) => {
                let _ = tx.send(GuiMessage::Core(CoreEvent::Warn(format!(
                    "获取课程失败: {error}"
                ))));
            }
        }
    });
}

fn apply_message(
    app: &MainWindow,
    shared: &Shared,
    tx: &mpsc::Sender<GuiMessage>,
    message: GuiMessage,
) {
    let logic = app.global::<Logic>();
    match message {
        GuiMessage::Core(event) => match event {
            CoreEvent::Info(text) => push_event(&logic, shared, EventKind::Info, "系统", text),
            CoreEvent::Warn(text) => {
                let lost = text.contains("登录失效");
                push_event(&logic, shared, EventKind::Warn, "系统", text.clone());
                if lost {
                    logic.set_session_valid(false);
                    stop_monitor_ui(&logic, shared);
                    push_event(&logic, shared, EventKind::Warn, "会话", format!("登录状态失效: {text}"));
                }
            }
            CoreEvent::Found {
                course_name,
                activity_id,
                kind,
                code,
                ..
            } => {
                logic.set_found_count(logic.get_found_count() + 1);
                let detail = match &code {
                    Some(code) => format!("{kind} · {code}"),
                    None => kind.clone(),
                };
                let mut state = shared.state.lock().unwrap();
                state.activities.insert(
                    0,
                    ActivityData {
                        id: activity_id,
                        course: course_name,
                        kind,
                        time: Local::now().format("%H:%M:%S").to_string(),
                        detail,
                        status: ActivityStatus::Detected,
                    },
                );
                if state.activities.len() > MAX_ACTIVITIES {
                    state.activities.truncate(MAX_ACTIVITIES);
                }
                drop(state);
                refresh_activity_views(&logic, shared);
            }
            CoreEvent::Signed {
                course_name,
                activity_id,
                result,
                ..
            } => {
                let (status, detail) = match result {
                    CheckInResult::Ok(message) => {
                        logic.set_signed_count(logic.get_signed_count() + 1);
                        (ActivityStatus::Success, message)
                    }
                    CheckInResult::Gone(message) => (ActivityStatus::Gone, message),
                    CheckInResult::Failed(message) => (ActivityStatus::Failed, message),
                };
                let mut state = shared.state.lock().unwrap();
                if let Some(row) = state.activities.iter_mut().find(|row| row.id == activity_id) {
                    row.status = status;
                    row.detail = detail;
                } else {
                    state.activities.insert(
                        0,
                        ActivityData {
                            id: activity_id,
                            course: course_name,
                            kind: String::new(),
                            time: Local::now().format("%H:%M:%S").to_string(),
                            detail,
                            status,
                        },
                    );
                    if state.activities.len() > MAX_ACTIVITIES {
                        state.activities.truncate(MAX_ACTIVITIES);
                    }
                }
                drop(state);
                refresh_activity_views(&logic, shared);
            }
        },
        GuiMessage::SessionCheck(valid) => {
            logic.set_session_valid(valid);
            if valid {
                push_event(&logic, shared, EventKind::Ok, "会话", "已恢复登录状态".to_string());
                spawn_fetch_courses(tx.clone());
            } else {
                push_event(
                    &logic,
                    shared,
                    EventKind::Warn,
                    "会话",
                    "保存的会话已失效,请重新登录".to_string(),
                );
            }
        }
        GuiMessage::LoginName(name) => logic.set_login_name(name.into()),
        GuiMessage::Courses(courses) => {
            logic.set_courses_count(courses.len() as i32);
            let rows: Vec<CourseRow> = courses
                .into_iter()
                .map(|course| CourseRow {
                    name: course.name.into(),
                })
                .collect();
            logic.set_courses(Rc::new(slint::VecModel::from(rows)).into());
        }
        GuiMessage::MonitorStopped(reason) => {
            if logic.get_monitoring() {
                stop_monitor_ui(&logic, shared);
                push_event(&logic, shared, EventKind::Warn, "系统", reason);
            }
        }
        GuiMessage::LoginOk => {
            logic.set_has_cookie(true);
            logic.set_session_valid(true);
            push_event(&logic, shared, EventKind::Ok, "会话", "微信登录成功".to_string());
            spawn_fetch_courses(tx.clone());
        }
        GuiMessage::LoginFail(reason) => {
            push_event(&logic, shared, EventKind::Warn, "会话", format!("登录失败: {reason}"));
        }
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    let total_seconds = elapsed.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        total_seconds / 3600,
        (total_seconds % 3600) / 60,
        total_seconds % 60
    )
}

fn main() {
    let app = MainWindow::new().expect("无法创建窗口");
    let shared = Arc::new(Shared::new());
    let logic = app.global::<Logic>();
    let app_weak = app.as_weak();

    let (tx, rx) = mpsc::channel::<GuiMessage>();

    // 事件泵:后台线程阻塞等消息,逐条投递到 UI 线程;空闲时零唤醒、零渲染。
    {
        let weak = app_weak.clone();
        let shared = shared.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            while let Ok(message) = rx.recv() {
                let weak = weak.clone();
                let shared = shared.clone();
                let tx = tx.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(app) = weak.upgrade() {
                        apply_message(&app, &shared, &tx, message);
                    }
                });
            }
        });
    }

    logic.set_has_cookie(!storage::load_config().cookie.is_empty());

    // 启动时校验已保存的会话
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let config = storage::load_config();
            if config.cookie.is_empty() {
                return;
            }
            let mut client = LiveClient::from_cookie_string(&config.cookie);
            let valid = client.check_login().is_ok();
            let _ = tx.send(GuiMessage::SessionCheck(valid));
        });
    }

    // 计时器:仅在监控期间每秒刷新耗时;发现已停止则自停,回到零唤醒。
    let elapsed_timer = Rc::new(slint::Timer::default());
    {
        let weak = app_weak.clone();
        let shared = shared.clone();
        let timer = elapsed_timer.clone();
        elapsed_timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), move || {
            let Some(app) = weak.upgrade() else {
                timer.stop();
                return;
            };
            if !app.global::<Logic>().get_monitoring() {
                timer.stop();
                return;
            }
            let started = *shared.started.lock().unwrap();
            if let Some(started) = started {
                app.global::<Logic>()
                    .set_elapsed(format_elapsed(started.elapsed()).into());
            }
        });
    }

    {
        let weak = app_weak.clone();
        // 无框窗口:系统级拖动 + 双击最大化 + 最小化/最大化/关闭
    {
        use std::cell::Cell;
        let weak = app_weak.clone();
        let last_press: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        logic.on_titlebar_press(move || {
            let Some(app) = weak.upgrade() else { return };
            let now = Instant::now();
            let double_click = matches!(last_press.get(), Some(t) if now.duration_since(t) < Duration::from_millis(400));
            last_press.set(Some(now));
            if double_click {
                let maximized = app.window().is_maximized();
                app.window().set_maximized(!maximized);
            } else {
                app.window().with_winit_window(|w| {
                    let _ = w.drag_window();
                });
            }
        });
    }
    {
        let weak = app_weak.clone();
        logic.on_win_minimize(move || {
            if let Some(app) = weak.upgrade() {
                app.window().set_minimized(true);
            }
        });
    }
    {
        let weak = app_weak.clone();
        logic.on_win_maximize(move || {
            if let Some(app) = weak.upgrade() {
                let maximized = app.window().is_maximized();
                app.window().set_maximized(!maximized);
            }
        });
    }
    {
        let weak = app_weak.clone();
        logic.on_win_close(move || {
            if let Some(app) = weak.upgrade() {
                let _ = app.window().hide();
            }
        });
    }

    logic.on_nav(move |page| {
            if let Some(app) = weak.upgrade() {
                app.global::<Logic>().set_active_page(page);
            }
        });
    }

    // 开始/停止监控
    {
        let weak = app_weak.clone();
        let shared = shared.clone();
        let timer = elapsed_timer.clone();
        let tx = tx.clone();
        logic.on_start_stop(move || {
            let Some(app) = weak.upgrade() else { return };
            let logic = app.global::<Logic>();
            if logic.get_monitoring() {
                stop_monitor_ui(&logic, &shared);
                return;
            }
            let flag = Arc::new(AtomicBool::new(false));
            *shared.stop_flag.lock().unwrap() = Some(flag.clone());
            *shared.started.lock().unwrap() = Some(Instant::now());
            logic.set_monitoring(true);
            logic.set_session_valid(logic.get_has_cookie());
            logic.set_elapsed("00:00:00".into());
            timer.start(slint::TimerMode::Repeated, Duration::from_secs(1), {
                let weak = weak.clone();
                let shared = shared.clone();
                let timer = timer.clone();
                move || {
                    let Some(app) = weak.upgrade() else {
                        timer.stop();
                        return;
                    };
                    if !app.global::<Logic>().get_monitoring() {
                        timer.stop();
                        return;
                    }
                    let started = *shared.started.lock().unwrap();
                    if let Some(started) = started {
                        app.global::<Logic>()
                            .set_elapsed(format_elapsed(started.elapsed()).into());
                    }
                }
            });
            spawn_monitor(tx.clone(), flag);
        });
    }

    {
        let weak = app_weak.clone();
        logic.on_open_login(move || {
            if let Some(app) = weak.upgrade() {
                app.global::<Logic>().set_login_open(true);
            }
        });
    }

    {
        let weak = app_weak.clone();
        logic.on_login_cancel(move || {
            if let Some(app) = weak.upgrade() {
                app.global::<Logic>().set_login_open(false);
            }
        });
    }

    // 提交登录:后台线程换 cookie,成功后保存并通知
    {
        let weak = app_weak.clone();
        let tx = tx.clone();
        logic.on_login_submit(move || {
            let Some(app) = weak.upgrade() else { return };
            let logic = app.global::<Logic>();
            let link = logic.get_login_link().to_string();
            logic.set_login_link("".into());
            logic.set_login_open(false);
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut client = LiveClient::new();
                let result = client
                    .login_wechat(&link)
                    .map_err(|error| error.to_string())
                    .and_then(|_| {
                        storage::save_cookie(&client.cookie_string()).map_err(|error| error.to_string())
                    });
                let _ = match result {
                    Ok(()) => tx.send(GuiMessage::LoginOk),
                    Err(error) => tx.send(GuiMessage::LoginFail(error)),
                };
            });
        });
    }

    logic.on_copy_auth_link(move || {
        if let Ok(mut clipboard) = arboard::Clipboard::new() {
            let _ = clipboard.set_text(WECHAT_AUTH_URL);
        }
    });

    // 退出登录:清空本地会话与全部状态
    {
        let weak = app_weak.clone();
        let shared = shared.clone();
        logic.on_logout(move || {
            let _ = storage::save_cookie("");
            if let Some(app) = weak.upgrade() {
                let logic = app.global::<Logic>();
                logic.set_has_cookie(false);
                logic.set_session_valid(false);
                logic.set_login_name("".into());
                logic.set_courses_count(0);
                logic.set_found_count(0);
                logic.set_signed_count(0);
                logic.set_courses(Rc::new(slint::VecModel::<CourseRow>::default()).into());
                {
                    let mut state = shared.state.lock().unwrap();
                    state.events.clear();
                    state.activities.clear();
                }
                refresh_activity_views(&logic, &shared);
                logic.set_events(Default::default());
                push_event(&logic, &shared, EventKind::Info, "会话", "已退出登录".to_string());
            }
        });
    }

    app.run().expect("事件循环错误");
}
