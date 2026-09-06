fn main() {
    slint_build::compile("ui/app.slint").unwrap();

    #[cfg(windows)]
    embed_windows_icon();
}

// 仅在 Windows 目标上嵌入应用图标(铅笔造型)到 exe 资源。
// embed-resource 在 Windows 上调用 Windows SDK 的 rc.exe 编译 app.rc
// (ICON 语句会展开为 RT_GROUP_ICON + RT_ICON),在其它宿主上不编译。
#[cfg(windows)]
fn embed_windows_icon() {
    if !std::path::Path::new("app.rc").exists() {
        panic!("app.rc not found; cannot embed the exe icon");
    }
    embed_resource::compile("app.rc", embed_resource::NONE);
    println!("cargo:rerun-if-changed=app.rc");
    println!("cargo:rerun-if-changed=assets/app.ico");
}
