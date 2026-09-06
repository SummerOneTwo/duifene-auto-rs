use std::path::PathBuf;
use std::process::Command;

fn main() {
    slint_build::compile("ui/app.slint").unwrap();

    #[cfg(windows)]
    embed_windows_icon();
}

// 仅在 Windows 目标上嵌入应用图标(铅笔造型)到 exe 资源。
// 用 llvm-rc (Rust/MSVC 工具链自带) 编译 app.rc -> resource.res,再用
// cargo:rustc-link-arg-bins 显式把 .res 注入到二进制链接。这样不依赖
// Windows SDK 的 rc.exe(之前资源编译成功但 .res 未被链接,导致无 RT_ICON)。
#[cfg(windows)]
fn embed_windows_icon() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default();
    let ico_path = std::path::Path::new(&manifest_dir).join("assets/app.ico");
    if !ico_path.exists() {
        panic!("app icon not found: {}", ico_path.display());
    }
    let out_dir = std::env::var("OUT_DIR").unwrap_or_default();
    let rc_path = std::path::Path::new(&out_dir).join("app.rc");
    let res_path = std::path::Path::new(&out_dir).join("resource.res");

    // 写 .rc,用正斜杠防止 rc 转义破坏路径
    let ico = ico_path.display().to_string().replace('\\', "/");
    std::fs::write(&rc_path, format!("1 ICON \"{ico}\"\n"))
        .expect("failed to write app.rc");

    // 找资源编译器: 优先 RC_PATH, 其次 llvm-rc, 再回退 rc.exe(在 PATH 或 SDK)
    let rc = std::env::var("RC_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("llvm-rc"));
    let mut cmd = Command::new(&rc);
    cmd.arg(format!("/fo{}", res_path.display()))
        .arg(format!("/I{}", manifest_dir))
        .arg(rc_path.display().to_string());
    let status = cmd.output();
    if status.is_err() {
        // llvm-rc 可能不存在,回退到 SDK 的 rc.exe
        let rc2 = std::env::var("RC_PATH")
            .or_else(|_| std::env::var("RC_PATH_FALLBACK"))
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("rc.exe"));
        let mut cmd2 = Command::new(&rc2);
        cmd2.arg(format!("/fo{}", res_path.display()))
            .arg(format!("/I{}", manifest_dir))
            .arg(rc_path.display().to_string());
        let status2 = cmd2.output().unwrap_or_else(|e| {
            panic!("no resource compiler found (tried {} and {}): {e}", rc.display(), rc2.display())
        });
        if !status2.status.success() {
            panic!(
                "resource compiler ({}) failed:\nstdout={}\nstderr={}\n",
                rc2.display(),
                String::from_utf8_lossy(&status2.stdout),
                String::from_utf8_lossy(&status2.stderr),
            );
        }
    } else {
        let status = status.unwrap();
        if !status.status.success() {
            panic!(
                "resource compiler ({}) failed:\nstdout={}\nstderr={}\n",
                rc.display(),
                String::from_utf8_lossy(&status.stdout),
                String::from_utf8_lossy(&status.stderr),
            );
        }
    }
    if !res_path.exists() {
        panic!("resource .res not produced");
    }
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=app.rc");
}
