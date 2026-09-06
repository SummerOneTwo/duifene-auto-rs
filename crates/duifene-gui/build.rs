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
    // 在 Windows 上,llvm-rc 可能在 PATH;若无则回退 rc.exe。
    // 注意: Windows job 走系统 rc.exe,它生成的 .res 是 MSVC COFF 格式。
    let mut compile = |compiler: &Path, res: &Path| -> bool {
        let out = Command::new(compiler)
            .arg(format!("/fo{}", res.display()))
            .arg(format!("/I{}", manifest_dir))
            .arg(rc_path.display().to_string())
            .output();
        match out {
            Ok(o) => {
                println!(
                    "resource compiler {} rc_out={} rc_err={}",
                    compiler.display(),
                    String::from_utf8_lossy(&o.stdout).trim(),
                    String::from_utf8_lossy(&o.stderr).trim(),
                );
                o.status.success()
            }
            Err(_) => false,
        }
    };
    let mut ok = compile(&rc, &res_path);
    if !ok {
        let rc2 = std::env::var("RC_PATH_FALLBACK")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("rc.exe"));
        ok = compile(&rc2, &res_path);
    }
    if !ok {
        panic!("no working resource compiler (tried llvm-rc and rc.exe)");
    }
    if !res_path.exists() {
        panic!("resource .res not produced");
    }
    // 诊断: 检查 .res 内是否含 RT_ICON(type 1)
    let bytes = std::fs::read(&res_path).unwrap_or_default();
    let has_icon = bytes.windows(6).any(|w| w == [0x00,0x00,0x00,0x00,0x01,0x00]);
    println!("resource.res size={} has_rt_icon_marker={}", bytes.len(), has_icon);
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=app.rc");
}
