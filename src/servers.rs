//! A snapshot of a server over SSH: what it runs, what is installed on it, and
//! how far behind on updates it is.
//!
//! One connection, one command. A short `sh` script goes in on standard input
//! and prints each answer under a `@@mainstone name@@` marker, which
//! [`parse`] takes apart again. Feeding the script to `sh -s` rather than
//! running it as the command means it works whatever the account's login
//! shell is. Nothing in it needs root or changes anything on the server: the
//! list of pending updates is apt's own, as of the last time the server
//! refreshed it (Ubuntu does that daily by itself).
//!
//! Ubuntu and Debian only for now. Another system still reports its name,
//! kernel and uptime, with a note that the package list is missing.

use std::cmp::Ordering;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::client;
use russh::keys::{HashAlg, PrivateKeyWithHashAlg, PublicKey, PublicKeyOrCertificate};
use serde::{Deserialize, Serialize};

/// How long to wait for the server to answer at all, and then for the
/// script to finish.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(120);

const SCRIPT: &str = r#"
section() { printf '\n@@mainstone %s@@\n' "$1"; }
section os-release; cat /etc/os-release 2>/dev/null
section hostname; hostname 2>/dev/null || uname -n
section kernel; uname -r
section arch; uname -m
section uptime; cat /proc/uptime 2>/dev/null
section packages; dpkg-query -W -f='${db:Status-Abbrev}\t${Package}\t${Version}\t${Architecture}\n' 2>/dev/null
section upgradable; apt list --upgradable 2>/dev/null
section reboot; if [ -f /var/run/reboot-required ]; then echo yes; cat /var/run/reboot-required.pkgs 2>/dev/null; else echo no; fi
section lists-updated; stat -c %Y /var/lib/apt/periodic/update-success-stamp 2>/dev/null || stat -c %Y /var/cache/apt/pkgcache.bin 2>/dev/null
section last-upgrade; grep '^Start-Date:' /var/log/apt/history.log 2>/dev/null | tail -n 1
section end
"#;

/// The last server a snapshot worked for, remembered in `config.json`. The
/// password and the key's passphrase are never saved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Sign in with a key file rather than a password.
    pub use_key: bool,
    pub key_path: String,
}

impl Default for ServerSettings {
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 22,
            user: String::new(),
            use_key: false,
            key_path: String::new(),
        }
    }
}

/// How to prove who we are. Neither is ever written to disk by the app.
#[derive(Clone)]
pub enum Auth {
    Password(String),
    /// An OpenSSH private key file, and its passphrase if it has one.
    Key { path: PathBuf, passphrase: String },
}

#[derive(Clone)]
pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    /// A host key fingerprint the user has just agreed to trust. A server
    /// not yet in `known_hosts` is only accepted when its key has exactly
    /// this fingerprint, and is then added to the file.
    pub trust: Option<String>,
}

/// Why a snapshot was not taken.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The server is not in `known_hosts`. The user is shown the
    /// fingerprint and asked whether to trust it.
    UnknownHost { fingerprint: String },
    /// Anything else, as a sentence for the window.
    Other(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Other(message)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub architecture: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upgrade {
    pub name: String,
    pub installed: String,
    pub available: String,
    /// Offered by a `-security` pocket, so it fixes a published vulnerability.
    pub security: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    /// What was typed to reach it, which may differ from its own name.
    pub address: String,
    pub taken: String,
    pub hostname: String,
    /// `PRETTY_NAME` from `/etc/os-release`, such as "Ubuntu 24.04.1 LTS".
    pub os: String,
    pub os_id: String,
    pub os_version: String,
    pub kernel: String,
    /// A newer kernel that is installed but not running, until a reboot.
    pub newer_kernel: Option<String>,
    pub architecture: String,
    pub uptime_days: Option<f64>,
    pub reboot_required: bool,
    /// The packages that asked for the reboot.
    pub reboot_packages: Vec<String>,
    /// When the server last refreshed its list of available updates; the
    /// pending updates are only as current as this.
    pub lists_updated: Option<String>,
    /// When apt last installed or upgraded anything.
    pub last_upgrade: Option<String>,
    pub packages: Vec<Package>,
    pub upgradable: Vec<Upgrade>,
    /// Things worth knowing about how complete this snapshot is.
    pub notes: Vec<String>,
}

impl Snapshot {
    pub fn security_updates(&self) -> usize {
        self.upgradable.iter().filter(|u| u.security).count()
    }

    /// The update waiting for a package, if there is one.
    pub fn upgrade_for(&self, name: &str) -> Option<&Upgrade> {
        self.upgradable.iter().find(|u| u.name == name)
    }
}

/// Connect, run the script, and take the answer apart. Blocking; it runs on a
/// [`crate::task::Task`] thread.
pub fn snapshot(target: &Target) -> Result<Snapshot, Failure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("Could not start the SSH client: {e}"))?;
    let output = runtime.block_on(run(target))?;
    let mut snapshot = parse(&output);
    snapshot.address = target.host.trim().to_owned();
    snapshot.taken = chrono::Local::now().format("%Y-%m-%d %H:%M").to_string();
    Ok(snapshot)
}

/// What the host key check made of the server, for after the handshake.
#[derive(Default)]
enum Verdict {
    #[default]
    NotChecked,
    Known,
    Unknown(String),
    Changed { fingerprint: String, line: usize },
    Unreadable(String),
}

struct Client {
    host: String,
    port: u16,
    trust: Option<String>,
    verdict: Arc<Mutex<Verdict>>,
}

impl client::Handler for Client {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let key = key.public_key();
        let verdict = check_host_key(&self.host, self.port, &key, self.trust.as_deref());
        let accept = matches!(verdict, Verdict::Known);
        if let Ok(mut slot) = self.verdict.lock() {
            *slot = verdict;
        }
        Ok(accept)
    }
}

fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

/// Check the server's key against `~/.ssh/known_hosts`, the file OpenSSH
/// uses, so a server already trusted with `ssh` is trusted here too and the
/// other way round.
fn check_host_key(host: &str, port: u16, key: &PublicKey, trust: Option<&str>) -> Verdict {
    use russh::keys::Error as KeyError;
    let seen = fingerprint(key);
    match russh::keys::check_known_hosts(host, port, key) {
        Ok(true) => Verdict::Known,
        Ok(false) if trust == Some(seen.as_str()) => {
            log::info!("trusting {host}:{port} with key {seen}");
            match russh::keys::known_hosts::learn_known_hosts(host, port, key) {
                Ok(()) => Verdict::Known,
                Err(err) => Verdict::Unreadable(format!(
                    "The key could not be added to ~/.ssh/known_hosts: {err}"
                )),
            }
        }
        Ok(false) => Verdict::Unknown(seen),
        Err(KeyError::KeyChanged { line }) => Verdict::Changed {
            fingerprint: seen,
            line,
        },
        Err(err) => Verdict::Unreadable(format!("~/.ssh/known_hosts could not be read: {err}")),
    }
}

async fn run(target: &Target) -> Result<String, Failure> {
    let host = target.host.trim();
    let user = target.user.trim();
    let verdict = Arc::new(Mutex::new(Verdict::default()));
    let handler = Client {
        host: host.to_owned(),
        port: target.port,
        trust: target.trust.clone(),
        verdict: verdict.clone(),
    };
    let config = Arc::new(client::Config {
        inactivity_timeout: Some(SCRIPT_TIMEOUT),
        ..Default::default()
    });

    // The key is read before connecting, so a wrong path or passphrase is
    // reported as that and not as the server turning us away.
    let key = match &target.auth {
        Auth::Key { path, passphrase } => {
            let passphrase = (!passphrase.is_empty()).then_some(passphrase.as_str());
            Some(load_key(path, passphrase)?)
        }
        Auth::Password(_) => None,
    };

    log::debug!("ssh: connecting to {user}@{host}:{}", target.port);
    let connect = client::connect(config, (host, target.port), handler);
    let connected = tokio::time::timeout(CONNECT_TIMEOUT, connect)
        .await
        .map_err(|_| format!("{host} did not answer on port {} within 15 seconds.", target.port))?;
    let mut session = match connected {
        Ok(session) => session,
        Err(err) => {
            let verdict = std::mem::take(&mut *verdict.lock().map_err(|_| "poisoned".to_owned())?);
            return Err(match verdict {
                Verdict::Unknown(fingerprint) => Failure::UnknownHost { fingerprint },
                Verdict::Changed { fingerprint, line } => Failure::Other(format!(
                    "{host}'s host key has changed since it was last seen (line {line} of ~/.ssh/known_hosts). It now presents {fingerprint}. This can mean the server was reinstalled, or that something is intercepting the connection. If the change is expected, remove the old line and try again."
                )),
                Verdict::Unreadable(message) => Failure::Other(message),
                Verdict::Known | Verdict::NotChecked => {
                    Failure::Other(format!("Could not connect to {host}: {err}"))
                }
            });
        }
    };

    let authenticated = match (&target.auth, key) {
        (Auth::Key { .. }, Some(key)) => {
            let hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(|e| format!("SSH error: {e}"))?
                .flatten();
            session
                .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await
                .map_err(|e| format!("SSH error: {e}"))?
                .success()
        }
        (Auth::Password(password), _) => authenticate_password(&mut session, user, password).await?,
        (Auth::Key { .. }, None) => unreachable!("the key is loaded above"),
    };
    if !authenticated {
        let how = match target.auth {
            Auth::Password(_) => "password",
            Auth::Key { .. } => "key",
        };
        return Err(Failure::Other(format!(
            "{host} did not accept the {how} for {user}."
        )));
    }
    log::debug!("ssh: signed in to {host}; running the snapshot script");

    let output = tokio::time::timeout(SCRIPT_TIMEOUT, exec(&session))
        .await
        .map_err(|_| format!("{host} took more than two minutes to answer."))??;
    session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await
        .ok();
    Ok(output)
}

fn load_key(path: &std::path::Path, passphrase: Option<&str>) -> Result<russh::keys::PrivateKey, Failure> {
    use russh::keys::Error as KeyError;
    let shown = crate::config::tilde(path);
    russh::keys::load_secret_key(path, passphrase).map_err(|err| {
        Failure::Other(match err {
            KeyError::KeyIsEncrypted => format!("{shown} is protected by a passphrase. Enter it and try again."),
            KeyError::IO(e) => format!("Could not read {shown}: {e}"),
            // A wrong passphrase comes back as a decryption error.
            other if passphrase.is_some() => {
                format!("Could not open {shown}. Check the passphrase. ({other})")
            }
            other => format!("{shown} is not a private key the app can read: {other}"),
        })
    })
}

/// Password authentication, falling back to keyboard-interactive. Some
/// servers, including Ubuntu's own default configuration in some releases,
/// ask for the password through keyboard-interactive instead of accepting it
/// as a password.
async fn authenticate_password(
    session: &mut client::Handle<Client>,
    user: &str,
    password: &str,
) -> Result<bool, Failure> {
    use russh::client::KeyboardInteractiveAuthResponse as Response;
    let ssh = |e: russh::Error| Failure::Other(format!("SSH error: {e}"));

    match session
        .authenticate_password(user, password)
        .await
        .map_err(ssh)?
    {
        russh::client::AuthResult::Success => return Ok(true),
        // Only try again where the server offers the other way: otherwise a
        // wrong password would count as two failed attempts against the
        // account rather than one.
        russh::client::AuthResult::Failure {
            remaining_methods, ..
        } if !remaining_methods.contains(&russh::MethodKind::KeyboardInteractive) => {
            return Ok(false);
        }
        russh::client::AuthResult::Failure { .. } => {}
    }
    let mut response = session
        .authenticate_keyboard_interactive_start(user, None)
        .await
        .map_err(ssh)?;
    // A server may send several rounds, including empty ones; give up after
    // a few rather than go round forever.
    for _ in 0..5 {
        match response {
            Response::Success => return Ok(true),
            Response::Failure { .. } => return Ok(false),
            Response::InfoRequest { prompts, .. } => {
                let answers = prompts.iter().map(|_| password.to_owned()).collect();
                response = session
                    .authenticate_keyboard_interactive_respond(answers)
                    .await
                    .map_err(ssh)?;
            }
        }
    }
    Ok(false)
}

async fn exec(session: &client::Handle<Client>) -> Result<String, Failure> {
    let ssh = |e: russh::Error| Failure::Other(format!("SSH error: {e}"));
    let mut channel = session.channel_open_session().await.map_err(ssh)?;
    channel.exec(true, "env LC_ALL=C sh -s").await.map_err(ssh)?;
    channel.data_bytes(SCRIPT.as_bytes().to_vec()).await.map_err(ssh)?;
    channel.eof().await.map_err(ssh)?;

    let mut stdout = Vec::new();
    let mut status = None;
    while let Some(message) = channel.wait().await {
        match message {
            russh::ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            russh::ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
            _ => {}
        }
    }
    let output = String::from_utf8_lossy(&stdout).into_owned();
    // The script ends by printing this; without it, something stopped it.
    if !output.contains("@@mainstone end@@") {
        let status = status.map_or_else(|| "no exit status".to_owned(), |s| format!("exit status {s}"));
        return Err(Failure::Other(format!(
            "The server ended the session before the snapshot finished ({status}). Is `sh` available to this account?"
        )));
    }
    Ok(output)
}

/// The script's output, split at its markers.
fn sections(output: &str) -> Vec<(&str, Vec<&str>)> {
    let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
    for line in output.lines() {
        if let Some(name) = line
            .strip_prefix("@@mainstone ")
            .and_then(|rest| rest.strip_suffix("@@"))
        {
            sections.push((name, Vec::new()));
        } else if let Some((_, lines)) = sections.last_mut()
            && !line.is_empty()
        {
            lines.push(line);
        }
    }
    sections
}

pub fn parse(output: &str) -> Snapshot {
    let mut s = Snapshot::default();
    let sections = sections(output);
    let get = |name: &str| -> &[&str] {
        sections
            .iter()
            .find(|(n, _)| *n == name)
            .map_or(&[], |(_, lines)| lines.as_slice())
    };
    let first = |name: &str| get(name).first().map(|l| l.trim().to_owned()).unwrap_or_default();

    for line in get("os-release") {
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').to_owned();
        match key.trim() {
            "PRETTY_NAME" => s.os = value,
            "ID" => s.os_id = value,
            "VERSION_ID" => s.os_version = value,
            _ => {}
        }
    }
    s.hostname = first("hostname");
    s.kernel = first("kernel");
    s.architecture = first("arch");
    s.uptime_days = first("uptime")
        .split_whitespace()
        .next()
        .and_then(|secs| secs.parse::<f64>().ok())
        .map(|secs| (secs / 86_400.0 * 10.0).round() / 10.0);

    s.packages = get("packages").iter().filter_map(|l| parse_package(l)).collect();
    s.upgradable = get("upgradable").iter().filter_map(|l| parse_upgrade(l)).collect();

    let reboot = get("reboot");
    s.reboot_required = reboot.first().is_some_and(|l| l.trim() == "yes");
    s.reboot_packages = reboot.iter().skip(1).map(|l| l.trim().to_owned()).collect();

    s.lists_updated = first("lists-updated")
        .parse::<i64>()
        .ok()
        .and_then(|secs| chrono::DateTime::from_timestamp(secs, 0))
        .map(|t| t.with_timezone(&chrono::Local).format("%Y-%m-%d %H:%M").to_string());
    s.last_upgrade = get("last-upgrade")
        .first()
        .and_then(|l| l.strip_prefix("Start-Date:"))
        .map(|d| d.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|d| !d.is_empty());

    s.newer_kernel = newer_kernel(&s.kernel, &s.packages);
    if s.newer_kernel.is_some() && !s.reboot_required {
        s.reboot_required = true;
    }

    if s.packages.is_empty() {
        s.notes.push(if s.os_id.is_empty() {
            "No package list: this does not look like Ubuntu or Debian.".to_owned()
        } else {
            format!("No package list: only Ubuntu and Debian are supported so far, and this is {}.", s.os)
        });
    } else if s.lists_updated.is_none() {
        s.notes.push(
            "The server has no record of refreshing its update lists, so the pending updates may be incomplete."
                .to_owned(),
        );
    }
    s
}

/// One line of `dpkg-query`, if it is an installed package.
fn parse_package(line: &str) -> Option<Package> {
    let mut fields = line.split('\t');
    let status = fields.next()?;
    // "ii " is installed; "rc " is removed with its settings left behind.
    if !status.starts_with("ii") {
        return None;
    }
    Some(Package {
        name: fields.next()?.to_owned(),
        version: fields.next()?.to_owned(),
        architecture: fields.next().unwrap_or_default().to_owned(),
    })
}

/// One line of `apt list --upgradable`, such as
/// `openssl/noble-updates,noble-security 3.0.13-0ubuntu3.5 amd64 [upgradable from: 3.0.13-0ubuntu3.4]`.
fn parse_upgrade(line: &str) -> Option<Upgrade> {
    let (name, rest) = line.split_once('/')?;
    let mut words = rest.split_whitespace();
    let suites = words.next()?;
    let available = words.next()?.to_owned();
    let installed = rest
        .split_once("[upgradable from: ")
        .and_then(|(_, from)| from.strip_suffix(']'))
        .unwrap_or_default()
        .to_owned();
    Some(Upgrade {
        name: name.to_owned(),
        installed,
        available,
        security: suites.split(',').any(|s| s.ends_with("-security")),
    })
}

/// The newest kernel installed, if it is newer than the one running.
fn newer_kernel(running: &str, packages: &[Package]) -> Option<String> {
    let flavour = running.splitn(3, '-').nth(2).unwrap_or_default();
    packages
        .iter()
        .filter_map(|p| p.name.strip_prefix("linux-image-"))
        .filter(|release| release.starts_with(|c: char| c.is_ascii_digit()))
        .filter(|release| flavour.is_empty() || release.ends_with(flavour))
        .max_by(|a, b| compare_versions(a, b))
        .filter(|newest| compare_versions(newest, running) == Ordering::Greater)
        .map(str::to_owned)
}

/// Compare two version strings piece by piece, numbers as numbers, which is
/// enough to order kernel releases such as `6.8.0-45-generic`.
fn compare_versions(a: &str, b: &str) -> Ordering {
    fn pieces(s: &str) -> Vec<Result<u64, &str>> {
        let mut out = Vec::new();
        let mut rest = s;
        while !rest.is_empty() {
            let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            if digits > 0 {
                out.push(Ok(rest[..digits].parse().unwrap_or(u64::MAX)));
                rest = &rest[digits..];
            } else {
                let end = rest.find(|c: char| c.is_ascii_digit()).unwrap_or(rest.len());
                out.push(Err(&rest[..end]));
                rest = &rest[end..];
            }
        }
        out
    }
    let (a, b) = (pieces(a), pieces(b));
    for (x, y) in a.iter().zip(&b) {
        let order = match (x, y) {
            (Ok(x), Ok(y)) => x.cmp(y),
            (Err(x), Err(y)) => x.cmp(y),
            (Ok(_), Err(_)) => Ordering::Greater,
            (Err(_), Ok(_)) => Ordering::Less,
        };
        if order != Ordering::Equal {
            return order;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    const UBUNTU: &str = "\
Welcome to a chatty profile script

@@mainstone os-release@@
PRETTY_NAME=\"Ubuntu 24.04.1 LTS\"
NAME=\"Ubuntu\"
VERSION_ID=\"24.04\"
ID=ubuntu

@@mainstone hostname@@
web01

@@mainstone kernel@@
6.8.0-45-generic

@@mainstone arch@@
x86_64

@@mainstone uptime@@
1036800.52 2000000.00

@@mainstone packages@@
ii \topenssl\t3.0.13-0ubuntu3.4\tamd64
rc \told-thing\t1.0\tamd64
ii \tlinux-image-6.8.0-45-generic\t6.8.0-45.45\tamd64
ii \tlinux-image-6.8.0-47-generic\t6.8.0-47.47\tamd64
ii \tlinux-image-generic\t6.8.0-47.47\tamd64

@@mainstone upgradable@@
Listing...
openssl/noble-updates,noble-security 3.0.13-0ubuntu3.5 amd64 [upgradable from: 3.0.13-0ubuntu3.4]
vim/noble-updates 2:9.1.0016-1ubuntu7.3 amd64 [upgradable from: 2:9.1.0016-1ubuntu7.2]

@@mainstone reboot@@
no

@@mainstone lists-updated@@
1759900000

@@mainstone last-upgrade@@
Start-Date: 2026-09-30  06:12:44

@@mainstone end@@
";

    #[test]
    fn an_ubuntu_server_is_read_in_full() {
        let s = parse(UBUNTU);
        assert_eq!(s.os, "Ubuntu 24.04.1 LTS");
        assert_eq!(s.os_id, "ubuntu");
        assert_eq!(s.os_version, "24.04");
        assert_eq!(s.hostname, "web01");
        assert_eq!(s.kernel, "6.8.0-45-generic");
        assert_eq!(s.architecture, "x86_64");
        assert_eq!(s.uptime_days, Some(12.0));
        assert_eq!(s.packages.len(), 4, "the removed package is left out");
        assert_eq!(s.packages[0].name, "openssl");
        assert_eq!(s.upgradable.len(), 2);
        assert_eq!(s.security_updates(), 1);
        assert_eq!(s.upgrade_for("openssl").unwrap().installed, "3.0.13-0ubuntu3.4");
        assert_eq!(s.upgrade_for("vim").unwrap().available, "2:9.1.0016-1ubuntu7.3");
        assert_eq!(s.last_upgrade.as_deref(), Some("2026-09-30 06:12:44"));
        assert!(s.lists_updated.is_some());
        assert!(s.notes.is_empty());
    }

    #[test]
    fn a_newer_installed_kernel_means_a_reboot_is_due() {
        let s = parse(UBUNTU);
        assert_eq!(s.newer_kernel.as_deref(), Some("6.8.0-47-generic"));
        assert!(s.reboot_required);
    }

    #[test]
    fn the_reboot_file_and_its_packages_are_read() {
        let s = parse("@@mainstone reboot@@\nyes\nlinux-base\nlibc6\n@@mainstone end@@\n");
        assert!(s.reboot_required);
        assert_eq!(s.reboot_packages, ["linux-base", "libc6"]);
    }

    #[test]
    fn a_system_without_dpkg_says_so() {
        let s = parse(
            "@@mainstone os-release@@\nPRETTY_NAME=\"Rocky Linux 9.4\"\nID=\"rocky\"\n@@mainstone packages@@\n@@mainstone end@@\n",
        );
        assert_eq!(s.os, "Rocky Linux 9.4");
        assert!(s.packages.is_empty());
        assert!(s.notes[0].contains("Rocky Linux 9.4"));
    }

    #[test]
    fn kernel_releases_are_ordered_by_number_not_text() {
        assert_eq!(compare_versions("6.8.0-100-generic", "6.8.0-99-generic"), Ordering::Greater);
        assert_eq!(compare_versions("5.15.0-1-generic", "6.8.0-1-generic"), Ordering::Less);
        assert_eq!(compare_versions("6.8.0-45-generic", "6.8.0-45-generic"), Ordering::Equal);
    }

    #[test]
    fn only_kernels_of_the_running_flavour_count() {
        let packages = [Package {
            name: "linux-image-6.8.0-50-lowlatency".into(),
            ..Default::default()
        }];
        assert_eq!(newer_kernel("6.8.0-45-generic", &packages), None);
    }
}
