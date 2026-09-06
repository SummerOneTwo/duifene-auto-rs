use std::env;
use std::time::{Duration, Instant};

use chrono::Local;

use duifene_core::api::{CheckInResult, Client};
use duifene_core::engine::{Engine, EngineConfig, Event};
use duifene_core::live::LiveClient;
use duifene_core::models::{SignActivity, SignType};
use duifene_core::runner::Runner;
use duifene_core::storage;

fn usage() {
    println!("用法:");
    println!("  dfe login <微信授权链接>             微信链接登录并保存会话,支持全部签到类型");
    println!("  dfe courses                         打印本学期课程列表");
    println!("  dfe probe                           对每门课做一次拉取,检查接口健康");
    println!("  dfe coords list                     打印已配置的课程坐标,配置为可选项");
    println!("  dfe coords set <课程名> <经度> <纬度>  为某门课配置坐标,不配置则自动探测");
    println!("  dfe watch [--delay <秒>] [--seconds <秒>]  监控全部课程");
    println!("    --delay     检测到签到后延迟多少秒再签,默认立即");
    println!("    --seconds   最多运行多少秒后自动停止");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        usage();
        return;
    }
    match args[1].as_str() {
        "login" => command_login(&args[2..]),
        "courses" => command_courses(),
        "watch" => command_watch(&args[2..]),
        "probe" => command_probe(),
        "coords" => command_coords(&args[2..]),
        _ => usage(),
    }
}

fn command_coords(arguments: &[String]) {
    if arguments.is_empty() {
        usage();
        return;
    }
    let config = storage::load_config();
    match arguments[0].as_str() {
        "list" => {
            if config.locations.is_empty() {
                println!("暂无课程坐标配置,定位签到会自动探测位置");
            }
            for (name, location) in &config.locations {
                println!("{name}: {} {}", location.longitude, location.latitude);
            }
        }
        "set" => {
            if arguments.len() < 4 {
                usage();
                return;
            }
            let longitude: f64 = match arguments[2].parse() {
                Ok(value) if (-180.0..=180.0).contains(&value) => value,
                _ => {
                    println!("经度无效: {}", arguments[2]);
                    return;
                }
            };
            let latitude: f64 = match arguments[3].parse() {
                Ok(value) if (-90.0..=90.0).contains(&value) => value,
                _ => {
                    println!("纬度无效: {}", arguments[3]);
                    return;
                }
            };
            match storage::save_course_location(&arguments[1], &arguments[2], &arguments[3]) {
                Ok(()) => println!(
                    "已保存课程坐标,为可选项: {} {} {}",
                    arguments[1], longitude, latitude
                ),
                Err(error) => println!("保存失败: {error}"),
            }
        }
        _ => usage(),
    }
}

fn command_probe() {
    let config = storage::load_config();
    if config.cookie.is_empty() {
        println!("没有保存的会话,请先执行 dfe login");
        return;
    }
    let mut client = LiveClient::from_cookie_string(&config.cookie);
    if let Err(error) = client.check_login() {
        println!("会话检查失败: {error}");
        return;
    }
    let courses = match client.fetch_courses() {
        Ok(courses) => courses,
        Err(error) => {
            println!("获取课程失败: {error}");
            return;
        }
    };
    let started = Instant::now();
    let results = client.fetch_all(&courses);
    let elapsed = started.elapsed();
    for (course, result) in courses.iter().zip(results) {
        match result {
            Ok(rows) => {
                println!("[OK] {}: {} 行", course.name, rows.len());
                if let Some(first) = rows.first() {
                    let row_coords = match (
                        first.longitude.parse::<f64>(),
                        first.latitude.parse::<f64>(),
                    ) {
                        (Ok(longitude), Ok(latitude)) => Some((longitude, latitude)),
                        _ => None,
                    };
                    let code = if first.checkin_code.is_empty() {
                        None
                    } else {
                        Some(first.checkin_code.clone())
                    };
                    let probe_activity = SignActivity {
                        id: first.id.clone(),
                        r#type: SignType::parse(&first.checkin_type),
                        code,
                        coordinate: None,
                    };
                    let page_coords = client.activity_coords(&probe_activity, course);
                    println!(
                        "     首行: ID={} 类型={} 码={} CanApply={} StatusID={} 状态名={} 已签={} 截止={} 发起={}",
                        first.id,
                        first.checkin_type,
                        first.checkin_code,
                        first.can_apply,
                        first.status_id,
                        first.status_name,
                        first.checkin_status,
                        first.apply_limit,
                        first.creater_at
                    );
                    println!("     坐标诊断: 行内={row_coords:?} 活动页={page_coords:?}");
                }
            }
            Err(error) => println!("[ERR] {}: {error}", course.name),
        }
    }
    println!(
        "拉取 {} 门课共耗时 {} ms",
        courses.len(),
        elapsed.as_millis()
    );
}

fn command_login(arguments: &[String]) {
    if arguments.is_empty() {
        usage();
        return;
    }
    let mut client = LiveClient::new();
    match client.login_wechat(&arguments[0]) {
        Ok(()) => {
            let cookie_string = client.cookie_string();
            match storage::save_cookie(&cookie_string) {
                Ok(()) => println!("登录成功,会话已保存"),
                Err(error) => println!("登录成功,但保存会话失败: {error}"),
            }
        }
        Err(error) => println!("登录失败: {error}"),
    }
}

fn command_courses() {
    let config = storage::load_config();
    if config.cookie.is_empty() {
        println!("没有保存的会话,请先执行 dfe login");
        return;
    }
    let mut client = LiveClient::from_cookie_string(&config.cookie);
    if let Err(error) = client.check_login() {
        println!("会话检查失败: {error}");
        return;
    }
    match client.fetch_courses() {
        Ok(courses) => {
            if courses.is_empty() {
                println!("未获取到课程");
            }
            for (index, course) in courses.iter().enumerate() {
                println!(
                    "{}. {} (课堂ID: {})",
                    index + 1,
                    course.name,
                    course.tclass_id
                );
            }
        }
        Err(error) => println!("获取课程失败: {error}"),
    }
}

fn command_watch(arguments: &[String]) {
    let mut delay_seconds = 0;
    let mut max_seconds: Option<u64> = None;
    let mut index = 0;
    while index < arguments.len() {
        if arguments[index] == "--delay" && index + 1 < arguments.len() {
            delay_seconds = arguments[index + 1].parse().unwrap_or(0);
            index += 2;
        } else if arguments[index] == "--seconds" && index + 1 < arguments.len() {
            max_seconds = arguments[index + 1].parse().ok();
            index += 2;
        } else {
            index += 1;
        }
    }

    let config = storage::load_config();
    if config.cookie.is_empty() {
        println!("没有保存的会话,请先执行 dfe login");
        return;
    }
    let mut client = LiveClient::from_cookie_string(&config.cookie);
    let courses = match client.fetch_courses() {
        Ok(courses) => courses,
        Err(error) => {
            println!("获取课程失败: {error}");
            return;
        }
    };
    if courses.is_empty() {
        println!("未获取到课程,请检查会话是否有效");
        return;
    }

    let engine_config = EngineConfig {
        delay_seconds,
        coords: storage::course_coordinates(&config),
        refresh_every: 300,
        signed_percent: config.signed_percent,
    };
    let engine = Engine::new(Box::new(client), courses.clone(), engine_config);
    let mut runner = Runner::new(engine, Duration::from_secs(2));
    let deadline = max_seconds.map(|seconds| Instant::now() + Duration::from_secs(seconds));
    println!("开始监控 {} 门课程,延迟 {delay_seconds} 秒", courses.len());
    runner.run_until(
        &mut |event| {
            if let Some(line) = render_event(&event) {
                println!("{line}");
            }
        },
        deadline,
    );
    println!("监控已停止");
}

fn render_event(event: &Event) -> Option<String> {
    let time = Local::now().format("%H:%M:%S");
    match event {
        Event::Info(message) => Some(format!("[{time}] [INFO] {message}")),
        Event::Warn(message) => Some(format!("[{time}] [WARN] {message}")),
        Event::Found {
            course_name,
            activity_id,
            kind,
            code,
        } => {
            let code_text = code
                .as_ref()
                .map(|value| format!(" 码:{value}"))
                .unwrap_or_default();
            Some(format!(
                "[{time}] [INFO] 发现待签到! [{course_name}] ID:{activity_id} 类型:{kind}{code_text}"
            ))
        }
        Event::Signed {
            course_name,
            activity_id,
            result,
        } => match result {
            CheckInResult::Ok(message) => Some(format!(
                "[{time}] [OK] 签到成功 [{course_name}] ID:{activity_id}: {message}"
            )),
            CheckInResult::Gone(_) => None,
            CheckInResult::Failed(message) => Some(format!(
                "[{time}] [ERROR] 签到失败 [{course_name}] ID:{activity_id}: {message}"
            )),
        },
    }
}
