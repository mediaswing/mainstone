//! Logging: warnings to the terminal, as before, and, when debug logging is
//! on, everything the app does to a file in its data directory.
//!
//! The file is for working out what went wrong after the fact. A release
//! build on Windows has no console, and a `.app` opened from the Finder has
//! nowhere to print, so without it a failed sign-in or export leaves nothing
//! behind but the status bar. It can be switched on under Settings, or for a
//! single run with `GCM_DEBUG=1`, which also catches anything that happens
//! before the window opens.
//!
//! What goes in it: each Graph request's method, path, status, time taken and
//! Microsoft's `request-id` (which Microsoft support asks for), the steps of
//! an export, and every background task's start and finish. Paths can hold
//! object IDs and sign-in names. What never goes in it: the client secret,
//! the access token, any password, or a request or response body.
//!
//! Only this app's own messages are written at debug level. The libraries
//! underneath are held at warnings, partly because they are noisy, and partly
//! so that nothing they choose to print at debug level, such as a request's
//! headers, can carry the token into the file.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Once the file grows past this, the next start of debug logging moves it to
/// `gcm-debug.log.1`, replacing the one before.
const MAX_SIZE: u64 = 5 * 1024 * 1024;

/// The crate's own messages, as opposed to its dependencies'.
const OWN_TARGET: &str = env!("CARGO_CRATE_NAME");

struct Logger {
    /// The terminal, filtered by `RUST_LOG` as usual (warnings by default).
    stderr: env_logger::Logger,
    file_on: AtomicBool,
    file: Mutex<Option<File>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

pub fn path() -> PathBuf {
    crate::config::data_dir().join("gcm-debug.log")
}

/// Whether `GCM_DEBUG` asks for debug logging regardless of the setting.
pub fn forced_by_environment() -> bool {
    std::env::var_os("GCM_DEBUG").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Install the logger. Called once, first thing in `main`.
pub fn init() {
    let stderr = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).build();
    let logger = LOGGER.get_or_init(|| Logger {
        stderr,
        file_on: AtomicBool::new(false),
        file: Mutex::new(None),
    });
    if log::set_logger(logger).is_ok() {
        update_max_level(logger);
    }

    // A panic in a release build would otherwise vanish with the window.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        log::error!("panic: {info}");
        previous(info);
    }));

    if forced_by_environment()
        && let Err(err) = set_debug(true)
    {
        log::warn!("{err}");
    }
}

/// Start or stop writing the debug log. Starting it again after a stop
/// carries on in the same file.
pub fn set_debug(on: bool) -> Result<(), String> {
    let Some(logger) = LOGGER.get() else {
        return Ok(());
    };
    if on == logger.file_on.load(Ordering::Relaxed) {
        return Ok(());
    }
    if on {
        let file = open().map_err(|e| {
            format!(
                "Could not open the debug log {}: {e}",
                crate::config::tilde(&path())
            )
        })?;
        if let Ok(mut slot) = logger.file.lock() {
            *slot = Some(file);
        }
        logger.file_on.store(true, Ordering::Relaxed);
        update_max_level(logger);
        log::info!(
            "debug logging started: mainstone {} on {} {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    } else {
        log::info!("debug logging stopped");
        logger.file_on.store(false, Ordering::Relaxed);
        if let Ok(mut slot) = logger.file.lock() {
            *slot = None;
        }
        update_max_level(logger);
    }
    Ok(())
}

/// Show the log in the file manager: the file itself selected where the
/// platform can do that, otherwise the folder it is in.
pub fn reveal() -> Result<(), String> {
    use std::process::Command;

    let file = path();
    let folder = crate::config::data_dir();
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let exists = file.exists();
    let mut command = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        if exists {
            c.arg("-R").arg(&file);
        } else {
            c.arg(&folder);
        }
        c
    } else if cfg!(windows) {
        let mut c = Command::new("explorer");
        if exists {
            // Explorer wants `/select,` and the path as one argument.
            let mut select = std::ffi::OsString::from("/select,");
            select.push(&file);
            c.arg(select);
        } else {
            c.arg(&folder);
        }
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(&folder);
        c
    };
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not open {}: {e}", crate::config::tilde(&folder)))?;
    // Waited for off the drawing thread, only so it is not left a zombie:
    // Explorer's exit code means nothing, and `open` returns at once anyway.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Open the log for appending, readable only by its owner, after moving an
/// overgrown one aside.
fn open() -> std::io::Result<File> {
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > MAX_SIZE) {
        std::fs::rename(&path, path.with_extension("log.1"))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(&path)
}

fn update_max_level(logger: &Logger) {
    let file = if logger.file_on.load(Ordering::Relaxed) {
        LevelFilter::Debug
    } else {
        LevelFilter::Off
    };
    log::set_max_level(logger.stderr.filter().max(file));
}

/// Whether a message belongs in the file: this app's own at debug level and
/// above, anyone else's only from warnings up.
fn wanted_in_file(metadata: &Metadata<'_>) -> bool {
    let target = metadata.target();
    let own = target
        .strip_prefix(OWN_TARGET)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"));
    metadata.level() <= if own { Level::Debug } else { Level::Warn }
}

impl Log for Logger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        self.stderr.enabled(metadata)
            || (self.file_on.load(Ordering::Relaxed) && wanted_in_file(metadata))
    }

    fn log(&self, record: &Record<'_>) {
        if self.stderr.matches(record) {
            self.stderr.log(record);
        }
        if !self.file_on.load(Ordering::Relaxed) || !wanted_in_file(record.metadata()) {
            return;
        }
        let line = format!(
            "{} {:<5} {}: {}\n",
            chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ"),
            record.level(),
            record.target(),
            record.args()
        );
        if let Ok(mut slot) = self.file.lock()
            && let Some(file) = slot.as_mut()
        {
            // Written straight through, so the last lines before a crash are
            // in the file rather than in a buffer that died with it.
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn flush(&self) {
        if let Ok(mut slot) = self.file.lock()
            && let Some(file) = slot.as_mut()
        {
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_apps_own_debug_messages_go_in_the_file() {
        let meta = |level, target| Metadata::builder().level(level).target(target).build();
        assert!(wanted_in_file(&meta(Level::Debug, "mainstone::graph")));
        assert!(wanted_in_file(&meta(Level::Debug, "mainstone")));
        assert!(!wanted_in_file(&meta(Level::Trace, "mainstone::graph")));
        assert!(!wanted_in_file(&meta(Level::Debug, "ureq::unversioned")));
        assert!(!wanted_in_file(&meta(Level::Info, "mainstonex")));
        assert!(wanted_in_file(&meta(Level::Warn, "rustls")));
    }
}
