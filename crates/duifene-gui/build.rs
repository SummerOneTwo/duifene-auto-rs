fn main() {
    slint_build::compile("ui/app.slint").unwrap();

    #[cfg(windows)]
    embed_windows_icon();
}

// 仅在 Windows 目标上嵌入应用图标(铅笔造型)到 exe 资源。
// winresource 用 Windows SDK 的 rc.exe 编译 .res(自动定位 SDK/vswhere),
// 它的 .res 经 cargo rustc-link-lib 链接,是 Windows 资源嵌入的成熟路径。
#[cfg(windows)]
fn embed_windows_icon() {
    if !std::path::Path::new("assets/app.ico").exists() {
        panic!("app icon not found: assets/app.ico");
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/app.ico");
    res.compile().expect("winresource failed to embed the exe icon");
    println!("cargo:rerun-if-changed=assets/app.ico");
}
