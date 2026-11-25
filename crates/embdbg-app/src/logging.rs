//! Logging to stderr and to a rolling file, so a crash that takes the window with it still leaves a trace.

use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::Context;
use directories::BaseDirs;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt};

/// Keeps the file writer's background thread alive for the rest of the process.
static FILE_WRITER: Mutex<Option<WorkerGuard>> = Mutex::new(None);

/// `%LOCALAPPDATA%\embdbg` (or the platform's equivalent): logs, and later settings.
pub fn app_data_dir() -> Option<PathBuf> {
    BaseDirs::new().map(|dirs| dirs.data_local_dir().join("embdbg"))
}

/// Sends panics to the log, where a window without a console can still be diagnosed from.
/// The default hook only prints to stderr, which is lost when the app is started from Explorer.
pub fn capture_panics() {
    std::panic::set_hook(Box::new(|info| {
        let location = info
            .location()
            .map_or_else(|| "unknown location".to_owned(), |at| at.to_string());
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "（没有文字说明）".to_owned());
        tracing::error!(target: "embdbg::panic", "panic at {location}: {message}");
    }));
}

pub fn init() -> anyhow::Result<()> {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,rmcp=warn"));
    let stderr = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(false)
        .compact();

    let Some(dir) = app_data_dir() else {
        tracing_subscriber::registry()
            .with(filter)
            .with(stderr)
            .init();
        return Ok(());
    };
    let log_dir = dir.join("logs");
    std::fs::create_dir_all(&log_dir)
        .with_context(|| format!("创建日志目录失败：{}", log_dir.display()))?;
    let appender = tracing_appender::rolling::daily(&log_dir, "embdbg.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let file = fmt::layer().with_ansi(false).with_writer(writer);

    tracing_subscriber::registry()
        .with(filter)
        .with(stderr)
        .with(file)
        .init();
    *FILE_WRITER.lock().unwrap() = Some(guard);
    Ok(())
}
