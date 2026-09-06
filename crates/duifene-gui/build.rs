use std::path::{Path, PathBuf};
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

    // 强制用 llvm-rc(windows runner 自带 LLVM 22.1)。
    // rc.exe 在部分新 Windows 环境下对 .ico 的 ICON 生成有问题,只生成 group 无位图。
    // llvm-rc 能正确展开 RT_ICON + RT_GROUP_ICON。
    let rc = std::env::var("RC_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("llvm-rc"));
    let out = Command::new(&rc)
        .arg(format!("/fo{}", res_path.display()))
        .arg(format!("/I{}", manifest_dir))
        .arg(rc_path.display().to_string())
        .output();
    let status = match out {
        Ok(o) => {
            println!(
                "cargo:warning=resource compiler={} rc_out={} rc_err={}",
                rc.display(),
                String::from_utf8_lossy(&o.stdout).trim(),
                String::from_utf8_lossy(&o.stderr).trim(),
            );
            o.status
        }
        Err(e) => panic!("failed to run {}: {e}", rc.display()),
    };
    if !status.success() {
        panic!("resource compiler ({}) failed", rc.display());
    }
    if !res_path.exists() {
        panic!("resource .res not produced");
    }
    // 诊断: 粗略统计 .res 里出现的数字资源类型(数字类型写成 u16(x)+u16(0))
    let bytes = std::fs::read(&res_path).unwrap_or_default();
    let mut type_ids = std::collections::HashSet::new();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        let val = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let zero = u16::from_le_bytes([bytes[i + 2], bytes[i + 3]]);
        if zero == 0 && (val == 1 || val == 14 || val == 3 || val == 16) {
            type_ids.insert(val);
        }
        i += 1;
    }
    let mut ids: Vec<u32> = type_ids.into_iter().map(|v| v as u32).collect();
    ids.sort();
    println!(
        "cargo:warning=resource.res size={} res_types={:?}",
        bytes.len(),
        ids
    );
    println!("cargo:rustc-link-arg-bins={}", res_path.display());
    println!("cargo:rerun-if-changed=assets/app.ico");
    println!("cargo:rerun-if-changed=app.rc");
}
