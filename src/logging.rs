use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Mutex;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::cli::Cli;
use crate::session::storage;

pub fn resolve_log_path(cli: &Cli) -> Option<PathBuf> {
    if let Some(ref path) = cli.log_file {
        return Some(path.clone());
    }
    if cli.verbose {
        let logs_dir = storage::data_dir().join("logs");
        fs::create_dir_all(&logs_dir).ok();
        let ts = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S");
        let pid = std::process::id();
        return Some(logs_dir.join(format!("zerostack-{ts}_{pid}.log")));
    }
    None
}

pub fn build_stderr_filter(cli: &Cli) -> EnvFilter {
    if let Some(ref lvl) = cli.log_level
        && let Ok(f) = EnvFilter::try_new(format!("{lvl},rig=off"))
    {
        return f;
    }
    if let Ok(f) = EnvFilter::try_from_default_env() {
        return f;
    }
    EnvFilter::new("warn,rig=off")
}

pub fn init(cli: &Cli) {
    let stderr_filter = build_stderr_filter(cli);
    let file_filter = EnvFilter::new("zerostack=trace,rig=off");

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .with_filter(stderr_filter);

    let registry = tracing_subscriber::registry().with(stderr_layer);

    let log_path = resolve_log_path(cli);
    if let Some(ref path) = log_path {
        match fs::File::create(path) {
            Ok(file) => {
                let file_layer = tracing_subscriber::fmt::layer()
                    .with_writer(Mutex::new(file))
                    .with_filter(file_filter);
                registry.with(file_layer).init();
                return;
            }
            Err(e) => {
                eprintln!(
                    "warning: could not create log file {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }

    registry.init();
}
