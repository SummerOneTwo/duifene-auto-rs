use std::env;
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;

use duifene_core::api::{ApiErr, CheckInResult, Client};
use duifene_core::email::{MailConfig, QQ_SMTP_HOST, QQ_SMTP_PORT};
use duifene_core::engine::{Engine, EngineConfig, Event};
use duifene_core::live::{LiveClient, WECHAT_AUTH_URL};
use duifene_core::models::{Course, SignActivity, SignType};
use duifene_core::runner::{random_interval_secs, Runner};
use duifene_core::schedule::{weekday_from_text, weekday_label, PollWindow};
use duifene_core::storage;

const DEFAULT_RELOGIN_POLL_SECS: u64 = 30;
const DEFAULT_RANDOM_MIN_SECS: u64 = 100;
const DEFAULT_RANDOM_MAX_SECS: u64 = 140;

fn usage() {
    println!("用法:");
    println!("  dfe login                         打印微信授权链接,按提示在微信中打开授权后登录");
    println!("  dfe login <微信授权链接>           用带 code= 的跳转链接登录并保存会话,支持全部签到类型");
    println!("  dfe courses                       打印本学期课程列表");
    println!("  dfe probe                         对每门课做一次拉取,检查接口健康");
    println!("  dfe coords list                   打印已配置的课程坐标(可选)");
    println!("  dfe coords set <课程名> <经度> <纬度>  为某门课配置坐标,不配置则自动探测");
    println!("  dfe schedule list                 打印高强度轮询窗口");
    println!("  dfe schedule clear                清空所有高强度轮询窗口");
    println!("  dfe schedule add [--weekday <1-7|周一..周日|mon..sun>]... --course <课程名/课堂ID> --start <HH:MM> --end <HH:MM> [--interval <秒>] [--warmup <分钟>]");
    println!("                                     课程在每周这些天的 [start, end] 内用短间隔(默认 2 秒)高频轮询;");
    println!("                                     --weekday 可重复(缺省每天); --warmup 提前 N 分钟开始高频;");
    println!("                                     --interval 默认 2 秒,便于测试也可设更大值");
    println!("  dfe percent                       查看全局签到门槛与各科覆盖");
    println!("  dfe percent <0-100>               设置全局签到门槛(0=关闭);已签到人数达到该百分比才签");
    println!("  dfe percent <课程名/课堂ID> <0-100>  设置某门课的签到门槛(覆盖全局);0=该课立即签");
    println!("  dfe percent <课程名/课堂ID> off     移除某门课的覆盖,回到全局值");
    println!("  dfe mail set [--smtp-host <主机>] [--port <端口>] <邮箱> <授权码>");
    println!("                                     保存 SMTP 邮件配置(默认 QQ smtp.qq.com:465)");
    println!("  dfe mail test                     发送一封测试邮件验证配置");
    println!("  dfe mail clear                    清除邮件配置");
    println!("  dfe daemon [--course <课程名/课堂ID>]... [--delay <秒>] [--min-secs <秒>] [--max-secs <秒>]");
    println!("                                     常驻服务:上课时间窗口内高强度轮询,其余时间慢速保活;");
    println!("                                     登录掉线/签到成功/失败会发邮件;每轮会重读窗口配置,改完无需重启");
    println!("      --course     只监控匹配指定名称或课堂ID的课程,可重复指定;缺省监控全部");
    println!("      --delay      检测到签到后延迟多少秒再签,默认立即");
    println!("      --min-secs / --max-secs  非窗口时段保活轮询随机范围(秒),默认 {DEFAULT_RANDOM_MIN_SECS}-{DEFAULT_RANDOM_MAX_SECS}");
    println!("  环境变量 DUIFENE_CONFIG_PATH 可指定配置路径,默认 ~/.config/duifene/config.json");
}

fn main() {
    // 过滤空参数(例如 systemd 展开空的 DUIFENE_DAEMON_ARGS)
    let args: Vec<String> = env::args().filter(|argument| !argument.is_empty()).collect();
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
        "schedule" => command_schedule(&args[2..]),
        "percent" => command_percent(&args[2..]),
        "mail" => command_mail(&args[2..]),
        "daemon" => command_daemon(&args[2..]),
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
                    "已保存课程坐标(可选): {} {} {}",
                    arguments[1], longitude, latitude
                ),
                Err(error) => println!("保存失败: {error}"),
            }
        }
        _ => usage(),
    }
}

/// 高强度轮询窗口管理:add / list / clear。
fn command_schedule(arguments: &[String]) {
    if arguments.is_empty() {
        usage();
        return;
    }
    match arguments[0].as_str() {
        "list" => {
            let windows = storage::load_config().poll_windows;
            if windows.is_empty() {
                println!("暂无高强度轮询窗口(课程在上课时间会用默认保活间隔)");
            }
            for (index, window) in windows.iter().enumerate() {
                let days = if window.weekdays.is_empty() {
                    "每天".to_string()
                } else {
                    window
                        .weekdays
                        .iter()
                        .map(|day| weekday_label(*day).to_string())
                        .collect::<Vec<_>>()
                        .join("/")
                };
                let course = if window.course.is_empty() {
                    "全部课程".to_string()
                } else {
                    window.course.clone()
                };
                let warmup = if window.warmup_minutes > 0 {
                    format!("(提前{}分钟)", window.warmup_minutes)
                } else {
                    String::new()
                };
                println!(
                    "{}. [{course}] {days} {}~{}{} 每{}秒",
                    index + 1,
                    window.start,
                    window.end,
                    warmup,
                    window.interval_secs
                );
            }
        }
        "clear" => match storage::save_poll_windows(&[]) {
            Ok(()) => println!("已清空高强度轮询窗口"),
            Err(error) => println!("清空失败: {error}"),
        },
        "add" => {
            let mut course = String::new();
            let mut weekdays: Vec<u8> = Vec::new();
            let mut start = String::new();
            let mut end = String::new();
            let mut interval_secs: u64 = 2;
            let mut warmup_minutes: u32 = 0;
            let mut index = 1;
            while index < arguments.len() {
                match arguments[index].as_str() {
                    "--course" if index + 1 < arguments.len() => {
                        course = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--weekday" if index + 1 < arguments.len() => {
                        match weekday_from_text(&arguments[index + 1]) {
                            Some(day) => {
                                if !weekdays.contains(&day) {
                                    weekdays.push(day);
                                }
                            }
                            None => {
                                println!("无效星期: {}", arguments[index + 1]);
                                return;
                            }
                        }
                        index += 2;
                    }
                    "--start" if index + 1 < arguments.len() => {
                        start = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--end" if index + 1 < arguments.len() => {
                        end = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--interval" if index + 1 < arguments.len() => {
                        interval_secs = match arguments[index + 1].parse() {
                            Ok(value) if value >= 1 => value,
                            _ => {
                                println!("无效间隔(需>=1秒): {}", arguments[index + 1]);
                                return;
                            }
                        };
                        index += 2;
                    }
                    "--warmup" if index + 1 < arguments.len() => {
                        warmup_minutes = match arguments[index + 1].parse() {
                            Ok(value) => value,
                            _ => {
                                println!("无效预热分钟数: {}", arguments[index + 1]);
                                return;
                            }
                        };
                        index += 2;
                    }
                    other => {
                        println!("未知参数: {other}");
                        return;
                    }
                }
            }
            if course.is_empty() || start.is_empty() || end.is_empty() {
                println!("用法: dfe schedule add --course <课程名/课堂ID> --start <HH:MM> --end <HH:MM> [--weekday ...] [--interval <秒>] [--warmup <分钟>]");
                return;
            }
            weekdays.sort_unstable();
            let window = PollWindow {
                course,
                weekdays,
                start,
                end,
                interval_secs,
                warmup_minutes,
            };
            if !window.is_valid() {
                println!("时间格式需为 HH:MM 且 end 不早于 start;星期取值 1-7");
                return;
            }
            let mut windows = storage::load_config().poll_windows;
            windows.push(window);
            match storage::save_poll_windows(&windows) {
                Ok(()) => {
                    println!("已添加窗口,daemon 每轮自动重读配置,无需重启");
                    println!("  执行 dfe schedule list 查看当前全部窗口");
                }
                Err(error) => println!("保存失败: {error}"),
            }
        }
        _ => usage(),
    }
}

/// 签到门槛(百分比)查看与设置:全局 + 单科覆盖。
fn command_percent(arguments: &[String]) {
    let config = storage::load_config();
    if arguments.is_empty() {
        println!("全局签到门槛: {}", config.signed_percent);
        if config.signed_percent_overrides.is_empty() {
            println!("  (无单科覆盖)");
        } else {
            println!("  单科覆盖:");
            for (course, percent) in &config.signed_percent_overrides {
                println!("    {course}: {percent}");
            }
        }
        println!("说明: 0=关闭(发现即签);>0=该课已签到人数达到百分比才签");
        return;
    }
    // 单个数字参数 => 设全局
    if arguments.len() == 1 {
        match arguments[0].parse::<u32>() {
            Ok(value) if (0..=100).contains(&value) => {
                match storage::save_signed_percent(value) {
                    Ok(()) => println!("全局签到门槛已设为 {value}"),
                    Err(error) => println!("保存失败: {error}"),
                }
            }
            _ => {
                println!("无效百分比(需 0-100): {}", arguments[0]);
            }
        }
        return;
    }
    // 两个参数 => 单科覆盖 或 off 移除
    let course = arguments[0].clone();
    let value = &arguments[1];
    if value.eq_ignore_ascii_case("off") {
        match storage::save_course_percent(&course, None) {
            Ok(()) => println!("已移除课程 [{course}] 的门槛覆盖,回到全局值"),
            Err(error) => println!("移除失败: {error}"),
        }
        return;
    }
    match value.parse::<u32>() {
        Ok(number) if (0..=100).contains(&number) => {
            match storage::save_course_percent(&course, Some(number)) {
                Ok(()) => println!("已设置课程 [{course}] 签到门槛为 {number}(覆盖全局)"),
                Err(error) => println!("保存失败: {error}"),
            }
        }
        _ => {
            println!("无效百分比(需 0-100 或 off): {value}");
        }
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
        println!("1) 在手机或电脑的微信中打开下面的授权链接(或复制到浏览器访问):");
        println!();
        println!("{WECHAT_AUTH_URL}");
        println!();
        println!("2) 授权并跳转后,复制浏览器地址栏里完整的链接(它带有 code= 参数);");
        println!("3) 在本服务器执行:  dfe login <完整链接>");
        return;
    }
    let link = &arguments[0];
    let mut client = LiveClient::new();
    match client.login_wechat(link) {
        Ok(()) => {
            let cookie_string = client.cookie_string();
            match storage::save_cookie(&cookie_string) {
                Ok(()) => {
                    let name = client
                        .login_name()
                        .ok()
                        .map(|value| format!(",登录账号 {value}"))
                        .unwrap_or_default();
                    println!("登录成功{name},会话已保存");
                }
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
        signed_percent_by_course: config.signed_percent_overrides.iter().map(|(k, v)| (k.clone(), *v)).collect(),
        watched_names: Vec::new(),
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

/// 处理邮件子命令:set / test / clear
fn command_mail(arguments: &[String]) {
    if arguments.is_empty() {
        usage();
        return;
    }
    match arguments[0].as_str() {
        "set" => {
            let mut host = QQ_SMTP_HOST.to_string();
            let mut port = QQ_SMTP_PORT;
            let mut index = 1;
            while index + 1 < arguments.len() {
                match arguments[index].as_str() {
                    "--smtp-host" => {
                        host = arguments[index + 1].clone();
                        index += 2;
                    }
                    "--port" => {
                        port = arguments[index + 1].parse().unwrap_or(port);
                        index += 2;
                    }
                    _ => break,
                }
            }
            if index + 1 >= arguments.len() {
                println!("用法: dfe mail set [--smtp-host <主机>] [--port <端口>] <邮箱> <授权码>");
                return;
            }
            let username = arguments[index].clone();
            let auth_code = arguments[index + 1].clone();
            if !username.contains('@') {
                println!("邮箱地址无效: {username}");
                return;
            }
            if auth_code.is_empty() {
                println!("授权码不能为空");
                return;
            }
            let mut mail = MailConfig {
                host,
                port,
                username: username.clone(),
                auth_code,
                from: None,
                to: None,
            };
            // 发件/收件默认都是该邮箱
            mail.from = Some(mail.username.clone());
            match storage::save_mail_config(Some(mail)) {
                Ok(()) => println!("邮件配置已保存,可执行 dfe mail test 验证"),
                Err(error) => println!("保存失败: {error}"),
            }
        }
        "test" => {
            let config = storage::load_config();
            let Some(mail) = config.mail else {
                println!("尚未配置邮件,先执行 dfe mail set <邮箱> <授权码>");
                return;
            };
            println!(
                "正在通过 {}:{} 发送测试邮件(可能耗时数秒)…",
                mail.host, mail.port
            );
            match mail.send_with_retry(
                "对分易自动签到 - 测试邮件",
                "这是一封来自对分易自动签到服务的测试邮件。\n如果你收到它,说明邮件通知配置正确。",
            ) {
                Ok(()) => println!("测试邮件已发送至 {}", mail.recipient()),
                Err(error) => {
                    println!("发送失败: {error}");
                    println!("提示:QQ 邮箱需在网页版「设置 → 账号 → 开启 SMTP」并生成授权码(非登录密码)。");
                }
            }
        }
        "clear" => match storage::save_mail_config(None) {
            Ok(()) => println!("已清除邮件配置"),
            Err(error) => println!("清除失败: {error}"),
        },
        _ => usage(),
    }
}

/// 常驻服务主入口。
/// daemon 单实例守卫:持有锁文件至进程结束。放在配置目录内,文件名由配置
/// 路径派生,不同账号(不同配置文件)互不冲突;异常退出残留的锁通过 pid
/// 存活检测自动回收。
struct DaemonGuard(#[allow(dead_code)] std::fs::File);

/// 判断一段 /proc/<pid>/cmdline 内容是否是 duifene-dfe 的 daemon 子命令。
/// 只认 daemon 模式,不影响 watch 等其他子命令并行运行。
fn cmdline_is_daemon(cmdline: &str) -> bool {
    let tokens: Vec<&str> = cmdline.split('\0').filter(|token| !token.is_empty()).collect();
    tokens.iter().any(|token| token.ends_with("duifene-dfe") || *token == "dfe")
        && tokens.iter().any(|token| *token == "daemon")
}

fn process_is_daemon(pid: u32) -> bool {
    let Ok(cmdline) = std::fs::read_to_string(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    cmdline_is_daemon(&cmdline)
}

fn acquire_daemon_lock() -> Result<DaemonGuard, String> {
    let config_path = storage::config_path_display();
    let lock_dir = std::path::Path::new(&config_path)
        .parent()
        .map(|parent| parent.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&config_path, &mut hasher);
    let key = std::hash::Hasher::finish(&hasher);
    let lock_path = lock_dir.join(format!("daemon-{key:016x}.lock"));
    try_lock_file(&lock_path)
}

fn try_lock_file(lock_path: &std::path::Path) -> Result<DaemonGuard, String> {
    // 已有锁:pid 仍是 daemon 则拒绝启动;进程已死则视为 stale 回收
    if let Ok(text) = std::fs::read_to_string(lock_path)
        && let Ok(pid) = text.trim().parse::<u32>()
        && process_is_daemon(pid)
    {
        return Err(format!("已有守护进程在运行(pid {pid}),本次启动退出"));
    }
    let _ = std::fs::remove_file(lock_path);
    let _ = std::fs::create_dir_all(lock_path.parent().unwrap_or(std::path::Path::new("/")));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(lock_path)
        .map_err(|error| format!("无法创建锁文件 {}: {error}", lock_path.display()))?;
    use std::io::Write as _;
    let _ = writeln!(file, "{}", std::process::id());
    Ok(DaemonGuard(file))
}

fn command_daemon(arguments: &[String]) {
    // 同配置只允许一个 daemon:曾出现模板单元(@u2/@u3)与主服务并存,
    // 造成多实例重复轮询;此锁保证任何途径拉起的重复实例立即退出。
    let _single_instance = match acquire_daemon_lock() {
        Ok(guard) => guard,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };
    // systemd 把环境变量 DUIFENE_DAEMON_ARGS 作为单个带空格的参数传入,
    // 先按空格拆成扁平列表,以便正确解析多个 --course / --delay 等。
    let mut flattened: Vec<String> = Vec::new();
    for argument in arguments {
        for token in argument.split_whitespace() {
            flattened.push(token.to_string());
        }
    }
    let arguments = &flattened;
    let mut watched_names: Vec<String> = Vec::new();
    let mut delay_seconds: i64 = 0;
    let mut min_secs = DEFAULT_RANDOM_MIN_SECS;
    let mut max_secs = DEFAULT_RANDOM_MAX_SECS;
    let mut index = 0;
    while index < arguments.len() {
        match arguments[index].as_str() {
            "--course" if index + 1 < arguments.len() => {
                watched_names.push(arguments[index + 1].clone());
                index += 2;
            }
            "--delay" if index + 1 < arguments.len() => {
                delay_seconds = arguments[index + 1].parse().unwrap_or(0);
                index += 2;
            }
            "--min-secs" if index + 1 < arguments.len() => {
                min_secs = arguments[index + 1].parse().unwrap_or(min_secs);
                index += 2;
            }
            "--max-secs" if index + 1 < arguments.len() => {
                max_secs = arguments[index + 1].parse().unwrap_or(max_secs);
                index += 2;
            }
            other => {
                eprintln!("忽略未知参数: {other}");
                index += 1;
            }
        }
    }

    println!("对分易自动签到守护进程启动");
    println!("  配置文件: {}", storage::config_path_display());
    if watched_names.is_empty() {
        println!("  监控范围: 全部课程");
    } else {
        println!("  监控范围: {}", watched_names.join(", "));
    }
    println!(
        "  非窗口时段间隔: 每轮随机 {min_secs}~{max_secs} 秒(保活);配置的课程窗口内自动切短间隔"
    );
    {
        // 启动即打印当前门槛,便于确认进程实际加载到的值(每轮还会热重载一次)
        let config = storage::load_config();
        if config.signed_percent == 0 && config.signed_percent_overrides.is_empty() {
            println!("  签到门槛: 关闭(发现即签)");
        } else {
            let mut parts = vec![format!("全局 {}%", config.signed_percent)];
            for (course, percent) in &config.signed_percent_overrides {
                parts.push(format!("{course} 覆盖 {percent}%"));
            }
            println!("  签到门槛: {}", parts.join(", "));
        }
    }

    // 外层:只要进程不退出就一直维护"登录 → 监控 → (掉线则等重登)"的循环
    loop {
        match run_monitor(&watched_names, delay_seconds, min_secs, max_secs) {
            MonitorEnd::LoginLost => {
                let mail = storage::load_config().mail;
                notify_login_lost(mail.as_ref());
                wait_for_relogin();
            }
            MonitorEnd::TransientRestart => {
                thread::sleep(Duration::from_secs(DEFAULT_RELOGIN_POLL_SECS));
            }
        }
    }
}

enum MonitorEnd {
    /// 引擎因登录失效(非网络原因)停止,需要重新微信授权
    LoginLost,
    /// 本轮因瞬时故障提前结束,稍后重试
    TransientRestart,
}

/// 维护一轮完整的"登录检查 → 拉课程 → 引擎动态调度监控",直到登录失效返回。
fn run_monitor(
    watched_names: &[String],
    delay_seconds: i64,
    min_secs: u64,
    max_secs: u64,
) -> MonitorEnd {
    // 阶段一:等待并校验会话
    loop {
        let config = storage::load_config();
        if config.cookie.is_empty() {
            wait_for_cookie();
            continue;
        }
        let mut client = LiveClient::from_cookie_string(&config.cookie);
        match client.check_login() {
            Ok(()) => break,
            Err(error) => {
                // 对分易把会话失效报成 Msg("登录已失效…"),网络故障是 Network;
                // 前者需重新授权,后者稍后重试即可
                let message = error.to_string();
                if matches!(error, ApiErr::AuthLost)
                    || (message.contains("登录") && !matches!(error, ApiErr::Network(_)))
                {
                    return MonitorEnd::LoginLost;
                }
                eprintln!("[{}] 会话检查暂不可用(非登录失效): {message}", now());
                thread::sleep(Duration::from_secs(DEFAULT_RELOGIN_POLL_SECS));
            }
        }
    }

    // 阶段二:拉取课程(瞬时错误则重试)
    let config = storage::load_config();
    let mut client = LiveClient::from_cookie_string(&config.cookie);
    let courses = loop {
        match client.fetch_courses() {
            Ok(courses) if !courses.is_empty() => break courses,
            Ok(_) => {
                eprintln!("[{}] 未获取到课程,请检查会话是否有效", now());
                thread::sleep(Duration::from_secs(DEFAULT_RELOGIN_POLL_SECS));
                continue;
            }
            Err(error) => {
                let message = error.to_string();
                if message.contains("登录") && !matches!(error, ApiErr::Network(_)) {
                    return MonitorEnd::LoginLost;
                }
                eprintln!("[{}] 获取课程失败: {message}", now());
                thread::sleep(Duration::from_secs(DEFAULT_RELOGIN_POLL_SECS));
                continue;
            }
        }
    };

    let engine_config = EngineConfig {
        delay_seconds,
        coords: storage::course_coordinates(&config),
        refresh_every: 300,
        signed_percent: config.signed_percent,
        signed_percent_by_course: config
            .signed_percent_overrides
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
        watched_names: watched_names.to_vec(),
    };
    // 上次已处理(签过/已结束/已通知失败)的活动注入初始集合,重启后不再重复处理
    let handled_initial = storage::handled_activities();
    let engine = Engine::with_handled(Box::new(client), courses, engine_config, handled_initial);
    if engine.watched_count() == 0 {
        eprintln!(
            "[{}] 警告: 课程过滤后没有可监控的课程,请检查 --course 参数或课程名",
            now()
        );
        return MonitorEnd::TransientRestart;
    }
    if let Ok(name) = engine_login_name(&storage::load_config().cookie) {
        println!("[{}] 已登录账号: {name}", now());
    }
    println!(
        "[{}] 开始常驻监控 {} 门课程(延迟 {delay_seconds} 秒)…",
        now(),
        engine.watched_count()
    );

    let mail = storage::load_config().mail;
    // 已通知失败的 ID 从磁盘恢复,重启后这些活动不再重发失败邮件
    let mut notified_failed: std::collections::HashSet<String> =
        storage::handled_activities();
    let watched_courses = engine.watched_courses();
    let mut runner = Runner::new(engine, Duration::from_secs(2));
    let mut login_lost = false;
    let mut last_phase = String::new();
    runner.run_dynamic(
        &mut |event| {
            handle_event(&event, mail.as_ref(), &mut notified_failed);
            if matches!(event, Event::Warn(message) if message.contains("登录失效")) {
                login_lost = true;
            }
        },
        &mut || {
            // 每轮重读窗口配置:上课窗口内课程切短间隔,其余回慢速保活
            let now_local = Local::now().naive_local();
            let interval = schedule_interval_secs(
                storage::load_config().poll_windows.as_slice(),
                watched_courses.clone(),
                now_local,
                min_secs,
                max_secs,
            );
            let phase = if interval.as_secs() < min_secs {
                format!("高频 {}s", interval.as_secs())
            } else {
                format!("保活 {}-{}s", min_secs, max_secs)
            };
            if phase != last_phase {
                println!("[{}] 轮询节奏: {phase}", now());
                last_phase = phase;
            }
            interval
        },
        |engine| {
            // OCR 取走本轮到达终态的活动ID,持久化到磁盘,重启后不再重复处理
            for id in engine.drain_newly_handled() {
                storage::add_handled_activity(&id);
            }
            // 每轮热重载签到门槛:dfe percent 改完后无需重启进程
            let latest = storage::load_config();
            engine.set_thresholds(
                latest.signed_percent,
                latest.signed_percent_overrides.into_iter().collect(),
            );
        },
    );
    if login_lost {
        eprintln!("[{}] 登录失效,引擎已停止,转入等待重新登录", now());
        MonitorEnd::LoginLost
    } else {
        eprintln!("[{}] 引擎意外结束,准备重启", now());
        MonitorEnd::TransientRestart
    }
}

/// 依据当前时间、课程与窗口配置决定本轮轮询间隔:
/// - 某课程的强化窗口生效 => 用窗口间隔(默认 2s);
/// - 否则 => 随机保活间隔 [min_secs, max_secs]。
fn schedule_interval_secs(
    windows: &[PollWindow],
    watched: Vec<Course>,
    now: chrono::NaiveDateTime,
    min_secs: u64,
    max_secs: u64,
) -> std::time::Duration {
    match duifene_core::schedule::active_interval_secs(windows, &watched, now) {
        Some(secs) => std::time::Duration::from_secs(secs),
        None => random_interval_secs(min_secs, max_secs),
    }
}

/// 独立用当前 cookie 请求一次登录名用于展示(失败静默)。
fn engine_login_name(cookie: &str) -> Result<String, ApiErr> {
    let mut client = LiveClient::from_cookie_string(cookie);
    client.login_name()
}

/// 处理单个引擎事件:控制台输出 + 邮件通知(成功/失败)。
fn handle_event(
    event: &Event,
    mail: Option<&MailConfig>,
    notified_failed: &mut std::collections::HashSet<String>,
) {
    if let Some(line) = render_event(event) {
        println!("{line}");
    }
    let Some(mail) = mail else {
        return;
    };
    match event {
        Event::Signed {
            course_name,
            activity_id,
            kind,
            result,
        } => match result {
            CheckInResult::Ok(message) => {
                let subject = format!("✅ 签到成功 [{course_name}] {kind}");
                let text = format!(
                    "课程: {course_name}\n类型: {kind}\n活动ID: {activity_id}\n结果: {message}\n时间: {}",
                    now()
                );
                let _ = mail.send_with_retry(&subject, &text);
            }
            CheckInResult::Failed(message) => {
                // 只用当前运行内存去重,加上磁盘记录:重启后已通知的失败不再重发
                if notified_failed.insert(activity_id.clone()) {
                    let subject = format!("⚠️ 签到失败 [{course_name}] {kind}");
                    let text = format!(
                        "课程: {course_name}\n类型: {kind}\n活动ID: {activity_id}\n原因: {message}\n时间: {}\n\n服务仍会按规则自动重试,无需手动处理。",
                        now()
                    );
                    let _ = mail.send_with_retry(&subject, &text);
                }
            }
            CheckInResult::Gone(_) => {}
        },
        _ => {}
    }
}

/// 发送"登录态掉线"通知邮件(尽力而为)。
fn notify_login_lost(mail: Option<&MailConfig>) {
    let Some(mail) = mail else {
        return;
    };
    let _ = mail.send_with_retry(
        "🔴 对分易登录态掉线,需要重新授权",
        &format!(
            "自动签到检测到登录态失效,服务已暂停签到。\n\n请在本服务器执行:\n  dfe login\n\n按提示用微信打开授权链接,再把跳转后带 code= 的完整链接作为参数执行:\n  dfe login <完整链接>\n\n服务会在检测到新会话后自动恢复。时间: {}",
            now()
        ),
    );
}

/// 会话失效后:周期读取配置,直到发现新的 cookie(用户重新 dfe login)后返回。
fn wait_for_relogin() {
    let last_cookie = storage::load_config().cookie;
    println!(
        "[{}] 等待重新登录:请执行  dfe login  (不带参数会打印授权链接)…",
        now()
    );
    loop {
        thread::sleep(Duration::from_secs(DEFAULT_RELOGIN_POLL_SECS));
        let current = storage::load_config().cookie;
        if !current.is_empty() && current != last_cookie {
            println!("[{}] 检测到新会话,重建监控…", now());
            if let Some(mail) = storage::load_config().mail {
                let _ = mail.send_with_retry(
                    "✅ 对分易已重新登录",
                    &format!("已检测到新的登录会话,自动签到服务已恢复。时间: {}", now()),
                );
            }
            return;
        }
    }
}

/// 首次尚无 cookie:周期读取,直到 dfe login 写入会话后返回。
fn wait_for_cookie() {
    println!(
        "[{}] 等待首次登录:请先执行  dfe login  完成微信授权登录…",
        now()
    );
    loop {
        thread::sleep(Duration::from_secs(DEFAULT_RELOGIN_POLL_SECS));
        if !storage::load_config().cookie.is_empty() {
            println!("[{}] 已检测到会话,启动监控…", now());
            return;
        }
    }
}

fn now() -> String {
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
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
            kind,
            result,
        } => match result {
            CheckInResult::Ok(message) => Some(format!(
                "[{time}] [OK] 签到成功 [{course_name}] {kind} ID:{activity_id}: {message}"
            )),
            CheckInResult::Gone(message) => Some(format!(
                "[{time}] [INFO] 已结束/不可签 [{course_name}] {kind} ID:{activity_id}: {message}"
            )),
            CheckInResult::Failed(message) => Some(format!(
                "[{time}] [ERROR] 签到失败 [{course_name}] {kind} ID:{activity_id}: {message}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cmdline_daemon_detection() {
        let daemon = "/usr/local/bin/duifene-dfe\0daemon\0--course\0软件工程\0";
        assert!(cmdline_is_daemon(daemon));
        let watch = "/usr/local/bin/duifene-dfe\0watch\0--seconds\0600\0";
        assert!(!cmdline_is_daemon(watch));
        let other = "/usr/bin/python3\0-u\0/root/duifene-monitor/monitor.py\0";
        assert!(!cmdline_is_daemon(other));
    }

    #[test]
    fn fresh_and_stale_lock_lifecycle() {
        let dir = std::env::temp_dir().join(format!("dfe-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lock = dir.join("daemon-test.lock");

        // 全新路径:直接取得锁,文件写入当前 pid
        let guard = try_lock_file(&lock).expect("首次加锁应成功");
        let text = std::fs::read_to_string(&lock).unwrap();
        assert_eq!(text.trim(), std::process::id().to_string());
        drop(guard);

        // 进程不存在(如 9999999)的 stale 锁:自动回收并重新加锁
        std::fs::write(&lock, "9999999").unwrap();
        let guard = try_lock_file(&lock).expect("stale 锁应被回收");
        let text = std::fs::read_to_string(&lock).unwrap();
        assert_eq!(text.trim(), std::process::id().to_string());
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
