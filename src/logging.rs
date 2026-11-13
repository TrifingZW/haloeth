//! 日志：写入应用数据目录下的 `Haloeth/logs/haloeth.log`，供用户与开发者排查问题。

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use tracing_subscriber::fmt::MakeWriter;

/// 日志目录；首次调用时创建。
pub fn log_dir() -> PathBuf {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        if let Some(base) = directories::BaseDirs::new() {
            let candidate = base.data_dir().join("Haloeth").join("logs");
            if std::fs::create_dir_all(&candidate).is_ok() {
                return candidate;
            }
        }
        std::env::temp_dir().join("Haloeth").join("logs")
    })
    .clone()
}

/// 日志文件路径。
pub fn log_file() -> PathBuf {
    log_dir().join("haloeth.log")
}

/// 追加写入同一个文件的多线程 writer。
#[derive(Clone)]
struct FileWriter(Arc<Mutex<std::fs::File>>);

impl Write for FileWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut file = self.0.lock().expect("日志文件不应被毒化");
        file.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let mut file = self.0.lock().expect("日志文件不应被毒化");
        file.flush()
    }
}

impl<'a> MakeWriter<'a> for FileWriter {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// 初始化日志订阅者；重复调用会被忽略。
pub fn init() {
    static INIT: OnceLock<()> = OnceLock::new();
    if INIT.set(()).is_err() {
        return;
    }
    let dir = log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let Ok(file) =
        std::fs::OpenOptions::new().create(true).append(true).open(dir.join("haloeth.log"))
    else {
        return;
    };
    let writer = FileWriter(Arc::new(Mutex::new(file)));
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_env_filter(filter)
        .with_ansi(false)
        .try_init();
    let path = log_file();
    tracing::info!("Haloeth {} 启动，日志写入 {}", env!("CARGO_PKG_VERSION"), path.display());
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        hook(info);
    }));
}

/// 用系统文件管理器打开日志目录。
pub fn open_log_dir() -> std::io::Result<()> {
    let dir = log_dir();
    std::fs::create_dir_all(&dir)?;
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program).arg(&dir).spawn().map(|_| ())
}
