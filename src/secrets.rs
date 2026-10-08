//! The two secrets the app can remember — the app registration's client
//! secret and the MariaDB password — kept in one JSON file in the home
//! directory, `~/.gcm-credentials.json`.
//!
//! The file is made read-only for its owner (mode 0400 on macOS and Linux,
//! the read-only attribute on Windows), and nobody else can read it. It is
//! plain JSON so that it can be written by hand or by a deployment script: a
//! file holding a tenant ID, client ID and client secret is enough for the app
//! to sign in by itself, with nothing typed into the window at all.
//!
//! It sits apart from `config.json` so the settings can be copied or shared
//! without the secrets going along with them.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Secrets {
    /// The tenant and client the secret belongs to, so a secret for one app
    /// registration is never sent while signing in as another.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tenant_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub mariadb_password: String,
}

impl Secrets {
    /// The client secret, if this file has one for this tenant and client.
    pub fn client_secret_for(&self, tenant_id: &str, client_id: &str) -> Option<&str> {
        let same = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
        (!self.client_secret.is_empty()
            && same(&self.tenant_id, tenant_id)
            && same(&self.client_id, client_id))
        .then_some(self.client_secret.as_str())
    }

    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

pub fn path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".gcm-credentials.json")
}

/// What the file holds, or nothing when it is absent or unreadable.
pub fn load() -> Secrets {
    let path = path();
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Secrets::default();
    };
    // Only the position of the fault is logged: serde's own message quotes
    // the offending value, and in this file that can be a secret.
    serde_json::from_str(&text).unwrap_or_else(|err| {
        log::warn!(
            "ignoring unreadable {} (line {}, column {})",
            path.display(),
            err.line(),
            err.column()
        );
        Secrets::default()
    })
}

/// Write the file, or remove it once there is nothing left in it.
pub fn save(secrets: &Secrets) -> Result<(), String> {
    let path = path();
    let shown = crate::config::tilde(&path);
    if secrets.is_empty() {
        if path.exists() {
            log::debug!("nothing left to remember; removing {}", path.display());
            set_read_only(&path, false).ok();
            std::fs::remove_file(&path)
                .map_err(|e| format!("Could not remove {shown}: {e}"))?;
        }
        return Ok(());
    }

    let json = serde_json::to_string_pretty(secrets).map_err(|e| e.to_string())?;
    // Which secrets, never what they are.
    log::debug!(
        "saving {} (client secret {}, MariaDB password {})",
        path.display(),
        !secrets.client_secret.is_empty(),
        !secrets.mariadb_password.is_empty()
    );
    // The file is read-only between runs, so it has to be made writable for
    // the moment it is being replaced.
    if path.exists() {
        set_read_only(&path, false).map_err(|e| format!("Could not update {shown}: {e}"))?;
    }
    crate::config::write_private(&path, json.as_bytes())
        .map_err(|e| format!("Could not write {shown}: {e}"))?;
    set_read_only(&path, true).map_err(|e| format!("Could not protect {shown}: {e}"))
}

#[cfg(unix)]
fn set_read_only(path: &std::path::Path, read_only: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if read_only { 0o400 } else { 0o600 };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_read_only(path: &std::path::Path, read_only: bool) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(read_only);
    std::fs::set_permissions(path, permissions)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_is_only_offered_to_its_own_app_registration() {
        let secrets = Secrets {
            tenant_id: "contoso.onmicrosoft.com".into(),
            client_id: "ABC".into(),
            client_secret: "s3cret".into(),
            ..Default::default()
        };
        assert_eq!(
            secrets.client_secret_for(" Contoso.onmicrosoft.com", "abc"),
            Some("s3cret")
        );
        assert_eq!(secrets.client_secret_for("contoso.onmicrosoft.com", "xyz"), None);
    }

    #[test]
    fn empty_fields_are_left_out_of_the_file() {
        let secrets = Secrets {
            mariadb_password: "pw".into(),
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_string(&secrets).unwrap(),
            r#"{"mariadb_password":"pw"}"#
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_file_can_still_be_replaced() {
        use std::os::unix::fs::PermissionsExt as _;
        let path = std::env::temp_dir().join(format!("gcm-ro-{}", std::process::id()));
        crate::config::write_private(&path, b"one").unwrap();
        set_read_only(&path, true).unwrap();
        set_read_only(&path, false).unwrap();
        crate::config::write_private(&path, b"two").unwrap();
        set_read_only(&path, true).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o400);
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        set_read_only(&path, false).unwrap();
        std::fs::remove_file(&path).unwrap();
    }
}
