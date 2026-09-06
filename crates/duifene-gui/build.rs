fn main() {
    slint_build::compile("ui/app.slint").unwrap();

    #[cfg(windows)]
    embed_windows_icon();
}

#[cfg(windows)]
fn embed_windows_icon() {
    if std::path::Path::new("assets/app.ico").exists() {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app.ico");
        let _ = res.compile();
    }
}
