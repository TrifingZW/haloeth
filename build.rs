//! 构建脚本：在 Windows 上把应用图标与版本信息写入可执行文件。

use std::fmt::Write as _;

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    #[cfg(windows)]
    {
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
        let mut parts =
            version.split('.').map(|part| part.parse::<u16>().unwrap_or(0)).collect::<Vec<_>>();
        parts.resize(4, 0);
        let (a, b, c, d) = (parts[0], parts[1], parts[2], parts[3]);
        let out = std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("app.rc");
        let icon = std::path::Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("dir"))
            .join("assets")
            .join("icon.ico");
        let mut rc = String::new();
        let _ = writeln!(rc, "1 ICON {:?}", icon.display().to_string().replace('\\', "/"));
        let _ = write!(
            rc,
            "\n1 VERSIONINFO\nFILEVERSION {a},{b},{c},{d}\nPRODUCTVERSION {a},{b},{c},{d}\n\
             FILEOS 0x40004\nFILETYPE 0x1\nBEGIN\n  BLOCK \"StringFileInfo\"\n  BEGIN\n    \
             BLOCK \"080404B0\"\n    BEGIN\n      VALUE \"FileDescription\", \"Haloeth\"\n      \
             VALUE \"FileVersion\", \"{version}\"\n      VALUE \"ProductName\", \"Haloeth\"\n      \
             VALUE \"ProductVersion\", \"{version}\"\n      VALUE \"LegalCopyright\", \"MIT OR Apache-2.0\"\n    \
             END\n  END\n  BLOCK \"VarFileInfo\"\n  BEGIN\n    VALUE \"Translation\", 0x0804, 1200\n  END\nEND\n"
        );
        std::fs::write(&out, rc).expect("write rc");
        let _ = embed_resource::compile(&out, embed_resource::NONE);
    }
}
