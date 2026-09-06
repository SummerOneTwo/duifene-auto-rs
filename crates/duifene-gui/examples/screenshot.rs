// 无头渲染截图示例(仅供开发预览,不参与发布构建):
//   cargo run --example screenshot -p duifene-gui
//   输出 target/screenshot-{home,courses,stats,login}.ppm,用 PIL 转 PNG 查看
use std::cell::OnceCell;
use std::rc::Rc;
use std::sync::OnceLock;

use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
use slint::platform::Platform;
use slint::winit_030::WinitWindowAccessor as _;

slint::include_modules!();

static WINDOW: OnceLock<()> = OnceLock::new();

thread_local! {
    static WINDOW_HANDLE: OnceCell<Rc<MinimalSoftwareWindow>> = OnceCell::new();
}

#[derive(Clone, Copy, Default)]
struct Rgb([u8; 3]);

impl slint::platform::software_renderer::TargetPixel for Rgb {
    fn blend(&mut self, color: slint::platform::software_renderer::PremultipliedRgbaColor) {
        let a = (u8::MAX - color.alpha) as u32;
        self.0[0] = ((self.0[0] as u32 * a + color.red as u32 * color.alpha as u32) / 255) as u8;
        self.0[1] = ((self.0[1] as u32 * a + color.green as u32 * color.alpha as u32) / 255) as u8;
        self.0[2] = ((self.0[2] as u32 * a + color.blue as u32 * color.alpha as u32) / 255) as u8;
    }
    fn from_rgb(red: u8, green: u8, blue: u8) -> Self {
        Rgb([red, green, blue])
    }
}

struct HeadlessPlatform;

impl Platform for HeadlessPlatform {
    fn create_window_adapter(
        &self,
    ) -> Result<Rc<dyn slint::platform::WindowAdapter>, slint::platform::PlatformError> {
        let window = MinimalSoftwareWindow::new(RepaintBufferType::NewBuffer);
        let _ = WINDOW.set(());
        WINDOW_HANDLE.with(|cell| {
            let _ = cell.set(window.clone());
        });
        Ok(window)
    }

    fn duration_since_start(&self) -> core::time::Duration {
        core::time::Duration::from_secs(160)
    }
}

const WIDTH: usize = 980;
const HEIGHT: usize = 640;

fn render_to_ppm(path: &str) {
    WINDOW_HANDLE.with(|cell| {
        let window = cell.get().unwrap();
        let mut buffer = vec![Rgb::default(); WIDTH * HEIGHT];
        window.draw_if_needed(|renderer| {
            renderer.render(&mut buffer, WIDTH);
        });
        let mut ppm = format!("P6\n{WIDTH} {HEIGHT}\n255\n").into_bytes();
        for pixel in buffer {
            ppm.extend_from_slice(&pixel.0);
        }
        std::fs::write(path, ppm).unwrap();
    });
}

fn main() {
    slint::platform::set_platform(Box::new(HeadlessPlatform)).unwrap();

    let app = MainWindow::new().unwrap();
    WINDOW_HANDLE.with(|cell| {
        cell.get()
            .unwrap()
            .set_size(slint::PhysicalSize::new(WIDTH as u32, HEIGHT as u32));
    });

    let logic = app.global::<Logic>();
    logic.set_has_cookie(true);
    logic.set_session_valid(true);
    logic.set_login_name("15625672568".into());
    logic.set_monitoring(true);
    logic.set_elapsed("00:02:40".into());
    logic.set_courses_count(3);
    logic.set_found_count(12);
    logic.set_signed_count(10);
    logic.set_courses(Rc::new(slint::VecModel::from(vec![
        CourseRow { name: "软件工程".into() },
        CourseRow { name: "数据结构与算法分析".into() },
        CourseRow { name: "计算机网络".into() },
    ]))
    .into());
    logic.set_activities(Rc::new(slint::VecModel::from(vec![
        ActivityRow {
            course: "软件工程".into(),
            kind: "数字码".into(),
            time: "13:00:40".into(),
            detail: "8848".into(),
            status: 0,
        },
        ActivityRow {
            course: "计算机网络".into(),
            kind: "定位".into(),
            time: "12:58:02".into(),
            detail: "".into(),
            status: 1,
        },
        ActivityRow {
            course: "数据结构与算法分析".into(),
            kind: "二维码".into(),
            time: "12:44:31".into(),
            detail: "已超出签到范围".into(),
            status: 2,
        },
    ]))
    .into());
    logic.set_events(Rc::new(slint::VecModel::from(vec![
        EventRow { time: "13:00:35".into(), kind: 2, course: "系统".into(), text: "持续监控中,共 3 门课".into() },
        EventRow { time: "13:00:31".into(), kind: 0, course: "会话".into(), text: "微信登录成功".into() },
        EventRow { time: "13:00:05".into(), kind: 1, course: "系统".into(), text: "登录状态失效: 服务器拒绝: 登录已失效: 当前为退出".into() },
        EventRow { time: "12:59:52".into(), kind: 1, course: "会话".into(), text: "保存的会话已失效,请重新登录".into() },
    ]))
    .into());
    logic.set_kind_stats(Rc::new(slint::VecModel::from(vec![
        StatRow { label: "数字码".into(), count: 6, ratio: 1.0, color: 0 },
        StatRow { label: "二维码".into(), count: 4, ratio: 0.66, color: 1 },
        StatRow { label: "定位".into(), count: 2, ratio: 0.33, color: 2 },
    ]))
    .into());
    logic.set_result_stats(Rc::new(slint::VecModel::from(vec![
        StatRow { label: "成功".into(), count: 10, ratio: 1.0, color: 1 },
        StatRow { label: "失败".into(), count: 2, ratio: 0.2, color: 3 },
    ]))
    .into());
    logic.set_course_stats(Rc::new(slint::VecModel::from(vec![
        StatRow { label: "软件工程".into(), count: 7, ratio: 1.0, color: 0 },
        StatRow { label: "计算机网络".into(), count: 3, ratio: 0.43, color: 1 },
        StatRow { label: "数据结构".into(), count: 2, ratio: 0.28, color: 2 },
    ]))
    .into());

    app.show().unwrap();
    render_to_ppm("target/screenshot-home.ppm");

    logic.set_active_page(1);
    render_to_ppm("target/screenshot-courses.ppm");
    logic.set_active_page(2);
    render_to_ppm("target/screenshot-stats.ppm");
    logic.set_active_page(0);
    logic.set_login_open(true);
    render_to_ppm("target/screenshot-login.ppm");

    let _ = app.window().hide();
}
