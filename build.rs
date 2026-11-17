//! 构建脚本：把应用图标与版本信息嵌入可执行文件。
//!
//! 资源脚本写入 `OUT_DIR`，由 `embed-resource` 编译并链接；非 Windows 目标由它判定为
//! `NotWindows` 后跳过，因此这里不按平台分支。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// 版本信息：数字版本用于资源字段，完整版本用于展示。
struct VersionInfo {
    /// 四段数字版本，供 `FILEVERSION` 与 `PRODUCTVERSION` 使用。
    numeric: String,
    /// `Cargo.toml` 中的完整版本字符串。
    text: String,
}

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");

    let out_dir = PathBuf::from(read_env("OUT_DIR"));
    let icon = Path::new(&read_env("CARGO_MANIFEST_DIR")).join("assets").join("icon.ico");
    let version = VersionInfo {
        numeric: format!(
            "{},{},{},0",
            read_env("CARGO_PKG_VERSION_MAJOR"),
            read_env("CARGO_PKG_VERSION_MINOR"),
            read_env("CARGO_PKG_VERSION_PATCH")
        ),
        text: read_env("CARGO_PKG_VERSION"),
    };

    let script = out_dir.join("app.rc");
    std::fs::write(&script, resource_script(&icon, &version)).expect("写入资源脚本 app.rc");
    embed_resource::compile(&script, embed_resource::NONE)
        .manifest_optional()
        .expect("嵌入 Windows 资源");
}

/// 读取构建期环境变量；缺失即视为构建配置错误。
fn read_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("缺少构建期环境变量 {name}"))
}

/// 生成资源脚本：应用图标与语言中性的版本信息。
fn resource_script(icon: &Path, version: &VersionInfo) -> String {
    let icon = icon.display().to_string().replace('\\', "/");
    let numeric = &version.numeric;
    let text = &version.text;
    let mut script = String::from("#pragma code_page(65001)\n");
    writeln!(script, "1 ICON \"{icon}\"").expect("写入图标资源");
    write!(
        script,
        "\n1 VERSIONINFO\n\
         FILEVERSION {numeric}\n\
         PRODUCTVERSION {numeric}\n\
         FILEOS 0x40004\n\
         FILETYPE 0x1\n\
         BEGIN\n  BLOCK \"StringFileInfo\"\n  BEGIN\n    BLOCK \"000004B0\"\n    BEGIN\n      \
         VALUE \"FileDescription\", \"Haloeth\"\n      \
         VALUE \"FileVersion\", \"{text}\"\n      \
         VALUE \"ProductName\", \"Haloeth\"\n      \
         VALUE \"ProductVersion\", \"{text}\"\n      \
         VALUE \"LegalCopyright\", \"MIT OR Apache-2.0\"\n    END\n  END\n  \
         BLOCK \"VarFileInfo\"\n  BEGIN\n    VALUE \"Translation\", 0x0000, 1200\n  END\nEND\n"
    )
    .expect("写入版本资源");
    script
}
