//! What the app remembers between runs: which tenant and app registration to
//! sign in as, where the MariaDB server is, the last server snapshotted over
//! SSH, light or dark, and whether to play sounds.
//!
//! Neither secret is in here. The client secret and the MariaDB password go in
//! a separate read-only file in the home directory (see [`crate::secrets`]),
//! so this one can be copied or shared without giving anything away.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::export::MariaDbSettings;
use crate::servers::ServerSettings;

/// Light, dark, or whatever this computer is set to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "Follow the system",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::System => "Light or dark to match the rest of the desktop, and change when it does.",
            Self::Light => "Always light, whatever the desktop is set to.",
            Self::Dark => "Always dark, whatever the desktop is set to.",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub appearance: Appearance,
    /// The directory (tenant) ID, a GUID or a verified domain name.
    pub tenant_id: String,
    /// The application (client) ID of the app registration.
    pub client_id: String,
    pub mariadb: MariaDbSettings,
    /// The server on the Servers tab.
    pub server: ServerSettings,
    /// Write a detailed log to `gcm-debug.log`; see [`crate::logging`]. On
    /// unless switched off.
    pub debug_logging: bool,
    /// Ask GitHub at startup whether there is a newer release; see
    /// [`crate::update`]. On unless switched off.
    pub check_for_updates: bool,
    /// A version the user chose to skip, so it is not offered again.
    pub skipped_update: Option<String>,
    /// Play the success and failure sounds; see [`crate::sound`]. On unless
    /// switched off.
    pub sounds: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            appearance: Appearance::default(),
            tenant_id: String::new(),
            client_id: String::new(),
            mariadb: MariaDbSettings::default(),
            server: ServerSettings::default(),
            debug_logging: true,
            check_for_updates: true,
            skipped_update: None,
            sounds: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            log::debug!("no settings at {}, starting fresh", path.display());
            return Self::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|err| {
            log::warn!(
                "ignoring unreadable config at {} (line {}, column {})",
                path.display(),
                err.line(),
                err.column()
            );
            Self::default()
        })
    }

    pub fn save(&self) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        let path = config_path();
        log::debug!("saving settings to {}", path.display());
        write_private(&path, json.as_bytes())
    }
}

/// Open a file for writing that only its owner can read, from the moment it
/// is created. A file that was already there is narrowed to the owner
/// *before* it is emptied and refilled, so the new contents are never
/// readable by anyone else, even for an instant.
///
/// On Windows there is no mode to set: the file takes the permissions of
/// the folder it is created in. Inside the user's profile, where the app's
/// own files live, that normally means the user and administrators only.
pub fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(path)?;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.set_len(0)?;
        Ok(file)
    }
    #[cfg(not(unix))]
    std::fs::File::create(path)
}

/// Write a file only its owner can read; see [`create_private`].
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = create_private(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// `~/Library/Application Support/GraphicalCloudManager` on macOS, the
/// equivalent elsewhere.
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("GraphicalCloudManager")
}

pub fn config_path() -> PathBuf {
    data_dir().join("config.json")
}

/// Shorten a path for display, so the window shows `~/…` rather than the
/// user's name.
pub fn tilde(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}
