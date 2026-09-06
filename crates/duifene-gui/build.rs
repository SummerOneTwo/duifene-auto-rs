use std::path::Path;

fn main() {
    slint_build::compile("ui/app.slint").unwrap();

    #[cfg(windows)]
    embed_windows_icon();
}

// 仅在 Windows 目标上嵌入应用图标(铅笔造型)到 exe 资源。
// rc.exe 编译 .rc 时工作目录不是 crates/duifene-gui,因此 .rc 里必须用
// 绝对路径引用 .ico,否则 ICON 语句会生成空的 group icon(无 RT_ICON),
// Windows 任务栏便回退到默认图标。
#[cfg(windows)]
fn embed_windows_icon() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let ico_abs = Path::new(&manifest_dir).join("assets/app.ico");
    if !ico_abs.exists() {
        panic!("app icon not found: {}", ico_abs.display());
    }
    // 写出带绝对路径的 .rc。rc.exe 里反斜杠是转义符,统一用正斜杠
    // (Windows API 同样接受 '/'),避免 "C:\api" 之类的路径被误解析。
    let rc_abs = Path::new(&manifest_dir).join("app.generated.rc");
    let ico_path = ico_abs.display().to_string().replace('\\', "/");
    let rc_body = format!("1 ICON \"{ico_path}\"\n");
    std::fs::write(&rc_abs, rc_body).expect("failed to write generated .rc");

    embed_resource::compile(rc_abs, embed_resource::NONE);
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=app.rc");
}
