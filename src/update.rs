//! Updating the app from its GitHub releases.
//!
//! At startup (unless it is switched off under Settings) the app asks GitHub
//! for the latest release of `mediaswing/mainstone`. If it is newer than this build,
//! a banner offers it. Installing it:
//!
//! 1. downloads the package for this platform from the release,
//! 2. checks it against the SHA-256 digest GitHub publishes for that file,
//!    and refuses it if they differ,
//! 3. puts it in place of this copy, and
//! 4. starts the new version and closes this one.
//!
//! What "in place" means depends on the platform:
//!
//! - macOS: the `.app` this is running from is swapped for the new one,
//!   in the same folder. The new bundle is ad-hoc signed like the old one, and
//!   as the app downloaded it itself it carries no quarantine flag, so it
//!   opens without Gatekeeper's prompt.
//! - Windows: the running `mainstone.exe` is renamed out of the way (Windows allows
//!   that, though not deleting it) and the new one copied to its name. The
//!   old one is deleted at the next start.
//! - Ubuntu: the `.deb` is installed with `apt-get` through `pkexec`, which
//!   asks for an administrator's password in the usual desktop dialog.
//!
//! A copy that was not installed from a release package — `cargo run`, say —
//! cannot be replaced like this, and is offered the release page instead.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

const LATEST: &str = "https://api.github.com/repos/mediaswing/mainstone/releases/latest";
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// The largest package that will be downloaded. Today's are under 10 MB.
const MAX_DOWNLOAD: u64 = 200 * 1024 * 1024;

/// A newer release, and the file in it for this platform.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: String,
    pub page: String,
    /// `None` when the release has no package for this platform, or this
    /// copy cannot replace itself; the banner then offers the page instead.
    pub package: Option<Package>,
}

#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// GitHub's SHA-256 of the file, as lowercase hex.
    pub sha256: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

#[derive(Deserialize)]
struct ApiAsset {
    name: String,
    browser_download_url: String,
    size: u64,
    /// "sha256:<hex>", for files uploaded since GitHub started computing it.
    digest: Option<String>,
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(300)))
        .timeout_connect(Some(Duration::from_secs(10)))
        .user_agent(concat!("mainstone/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// Ask GitHub whether there is a release newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    check_against(CURRENT, installed_as().is_some())
}

/// [`check`], for a given version and whether this copy can replace itself.
fn check_against(current: &str, replaceable: bool) -> Result<Option<Release>, String> {
    let mut response = agent()
        .get(LATEST)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| format!("Could not check for updates: {e}"))?;
    let release: ApiRelease = response
        .body_mut()
        .with_config()
        .limit(4 * 1024 * 1024)
        .read_json()
        .map_err(|e| format!("Unexpected answer from GitHub: {e}"))?;

    let version = release.tag_name.trim_start_matches('v').to_owned();
    if release.draft || release.prerelease || !is_newer(&version, current) {
        log::debug!("no update: latest release is {version}, this is {current}");
        return Ok(None);
    }

    let wanted = package_name(&version);
    let package = match &wanted {
        Some(wanted) if replaceable => release
            .assets
            .iter()
            .find(|a| &a.name == wanted)
            .and_then(|a| {
                let sha256 = a.digest.as_deref()?.strip_prefix("sha256:")?.to_lowercase();
                Some(Package {
                    name: a.name.clone(),
                    url: a.browser_download_url.clone(),
                    size: a.size,
                    sha256,
                })
            }),
        _ => None,
    };
    log::info!(
        "update available: {version} (this is {current}); {}",
        package
            .as_ref()
            .map_or("no installable package for this copy".to_owned(), |p| p.name.clone())
    );
    Ok(Some(Release {
        version,
        page: release.html_url,
        package,
    }))
}

/// `1.10.0` is newer than `1.9.2`. Anything that is not three numbers is
/// never newer, so a strangely named tag cannot cause an update loop.
fn is_newer(candidate: &str, current: &str) -> bool {
    fn parse(v: &str) -> Option<(u64, u64, u64)> {
        let mut parts = v.split('.').map(|p| p.parse::<u64>().ok());
        let parsed = (parts.next()??, parts.next()??, parts.next()??);
        parts.next().is_none().then_some(parsed)
    }
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// The release file for this platform, as `mainstone-package` names it.
fn package_name(version: &str) -> Option<String> {
    let arch = std::env::consts::ARCH;
    if cfg!(target_os = "macos") {
        Some(format!("mainstone-{version}-macos-{arch}.zip"))
    } else if cfg!(windows) {
        Some(format!("mainstone-{version}-windows-{arch}.zip"))
    } else if cfg!(target_os = "linux") {
        let deb_arch = match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            other => other,
        };
        Some(format!("mainstone-{version}-{deb_arch}.deb"))
    } else {
        None
    }
}

/// What this copy is installed as, and so what an update replaces: the
/// `.app` on macOS, `mainstone.exe` on Windows, the package's `/usr/bin/mainstone` on
/// Linux. `None` when it is not something an update can replace.
fn installed_as() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    if cfg!(target_os = "macos") {
        // …/Mainstone Cloud System.app/Contents/MacOS/mainstone
        let app = exe.parent()?.parent()?.parent()?;
        (app.extension()? == "app").then(|| app.to_path_buf())
    } else if cfg!(windows) {
        Some(exe)
    } else if cfg!(target_os = "linux") {
        (exe == Path::new("/usr/bin/mainstone")).then_some(exe)
    } else {
        None
    }
}

/// Download, check, install, and start the new version. `progress` is a line
/// for the banner. On success the caller closes this copy.
pub fn install(package: &Package, progress: &Arc<Mutex<String>>) -> Result<(), String> {
    let say = |text: String| {
        if let Ok(mut p) = progress.lock() {
            *p = text;
        }
    };
    let target = installed_as().ok_or("This copy of the app was not installed from a release package, so it cannot update itself.")?;

    // A new folder with a name nobody can guess. On Linux the temp folder is
    // shared, and a folder someone else made in advance would let them swap
    // the package between its checksum passing and pkexec installing it as
    // root; `create_dir` refuses one that is already there.
    let work = std::env::temp_dir().join(format!(
        "mainstone-update-{}-{:016x}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir(&work).map_err(|e| format!("Could not make {}: {e}", work.display()))?;
    let file = work.join(&package.name);
    let result = (|| {
        download(package, &file, &say)?;
        say("Installing…".into());
        install_file(&file, &target, &work)?;
        relaunch(&target)
    })();
    // The download is no use once installed, or if installing failed.
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn download(package: &Package, to: &Path, say: &dyn Fn(String)) -> Result<(), String> {
    if package.size > MAX_DOWNLOAD {
        return Err(format!("{} is larger than expected; not downloading it.", package.name));
    }
    log::info!("downloading {}", package.url);
    let mut response = agent()
        .get(&package.url)
        .call()
        .map_err(|e| format!("Could not download the update: {e}"))?;
    let mut reader = response
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD)
        .reader();
    let mut out = std::fs::File::create(to).map_err(|e| format!("Could not save the update: {e}"))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    let mut done: u64 = 0;
    loop {
        let n = reader
            .read(&mut buffer)
            .map_err(|e| format!("The download was interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        out.write_all(&buffer[..n])
            .map_err(|e| format!("Could not save the update: {e}"))?;
        done += n as u64;
        if let Some(percent) = (done * 100).checked_div(package.size) {
            say(format!("Downloading the update… {percent}%"));
        }
    }
    out.sync_all().map_err(|e| e.to_string())?;

    let actual = hex(&hasher.finalize());
    if actual != package.sha256 {
        log::warn!(
            "update digest mismatch for {}: expected {}, got {actual}",
            package.name,
            package.sha256
        );
        return Err("The downloaded update does not match the checksum GitHub published for it, so it was not installed.".into());
    }
    log::info!("{} downloaded and verified ({done} bytes)", package.name);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn run(command: &mut Command) -> Result<(), String> {
    let shown = format!("{command:?}");
    let output = command
        .output()
        .map_err(|e| format!("Could not start {shown}: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        log::warn!("{shown} failed ({}): {stderr}", output.status);
        Err(format!(
            "Installing the update failed: {}",
            stderr.lines().last().unwrap_or("unknown error").trim()
        ))
    }
}

#[cfg(target_os = "macos")]
fn install_file(zip: &Path, app: &Path, _work: &Path) -> Result<(), String> {
    let parent = app.parent().ok_or("The app has no folder.")?;
    // Unpacked next to the app, so the swap below is two renames on one
    // volume rather than a copy that could be half done.
    let staging = parent.join(format!(".mainstone-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    let result = (|| {
        // ditto keeps the bundle's signature and structure intact.
        run(Command::new("/usr/bin/ditto").arg("-x").arg("-k").arg(zip).arg(&staging))?;
        let new_app = std::fs::read_dir(&staging)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| p.extension().is_some_and(|x| x == "app"))
            .ok_or("The update has no app in it.")?;
        let old = parent.join(format!(".mainstone-old-{}.app", std::process::id()));
        std::fs::rename(app, &old).map_err(|e| {
            format!(
                "Could not replace {} ({e}). Move the app somewhere you can write to, such as Applications, and try again.",
                app.display()
            )
        })?;
        if let Err(e) = std::fs::rename(&new_app, app) {
            // Put the old one back rather than leave nothing there.
            let _ = std::fs::rename(&old, app);
            return Err(format!("Could not put the new version in place: {e}"));
        }
        let _ = std::fs::remove_dir_all(&old);
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&staging);
    result
}

#[cfg(target_os = "macos")]
fn relaunch(app: &Path) -> Result<(), String> {
    // -n: a new instance, even though this one is still running.
    Command::new("/usr/bin/open")
        .arg("-n")
        .arg(app)
        .spawn()
        .map(drop)
        .map_err(|e| format!("Updated, but could not start the new version: {e}"))
}

#[cfg(windows)]
fn install_file(zip: &Path, exe: &Path, work: &Path) -> Result<(), String> {
    // Windows' own tar unpacks zips, as mainstone-package uses it to make them.
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let tar = PathBuf::from(system_root).join("System32").join("tar.exe");
    let unpacked = work.join("unpacked");
    std::fs::create_dir_all(&unpacked).map_err(|e| e.to_string())?;
    run(Command::new(tar).arg("-x").arg("-f").arg(zip).arg("-C").arg(&unpacked))?;
    let new_exe = find_file(&unpacked, "mainstone.exe").ok_or("The update has no mainstone.exe in it.")?;

    let old = old_exe(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| {
        format!(
            "Could not replace {} ({e}). Put mainstone.exe somewhere you can write to and try again.",
            exe.display()
        )
    })?;
    if let Err(e) = std::fs::copy(&new_exe, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("Could not put the new version in place: {e}"));
    }
    Ok(())
}

#[cfg(windows)]
fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|n| n.eq_ignore_ascii_case(name)) {
            return Some(path);
        }
    }
    None
}

#[cfg(windows)]
fn old_exe(exe: &Path) -> PathBuf {
    exe.with_extension("exe.old")
}

#[cfg(windows)]
fn relaunch(exe: &Path) -> Result<(), String> {
    Command::new(exe)
        .spawn()
        .map(drop)
        .map_err(|e| format!("Updated, but could not start the new version: {e}"))
}

#[cfg(target_os = "linux")]
fn install_file(deb: &Path, _exe: &Path, _work: &Path) -> Result<(), String> {
    // The file has to be readable by apt's own unprivileged user too.
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(deb, std::fs::Permissions::from_mode(0o644));
        if let Some(dir) = deb.parent() {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("Could not prepare {} for installing: {e}", dir.display()))?;
        }
    }
    if !Path::new("/usr/bin/pkexec").exists() {
        return Err(format!(
            "pkexec is not installed, so the update cannot ask for your password. Install it with: sudo apt install {}",
            deb.display()
        ));
    }
    run(Command::new("/usr/bin/pkexec")
        .arg("/usr/bin/apt-get")
        .arg("install")
        .arg("-y")
        .arg(deb))
}

#[cfg(target_os = "linux")]
fn relaunch(exe: &Path) -> Result<(), String> {
    Command::new(exe)
        .spawn()
        .map(drop)
        .map_err(|e| format!("Updated, but could not start the new version: {e}"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn install_file(_: &Path, _: &Path, _: &Path) -> Result<(), String> {
    Err("Updating in place is not supported on this platform.".into())
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn relaunch(_: &Path) -> Result<(), String> {
    Ok(())
}

/// Tidy up after an update: on Windows, the old executable that could not be
/// deleted while it was running.
pub fn clean_up() {
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        let old = old_exe(&exe);
        if old.exists() {
            match std::fs::remove_file(&old) {
                Ok(()) => log::info!("removed {}", old.display()),
                Err(e) => log::debug!("could not remove {} yet: {e}", old.display()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_higher_version_is_newer() {
        assert!(is_newer("1.10.0", "1.9.2"));
        assert!(is_newer("2.0.0", "1.99.99"));
        assert!(!is_newer("1.2.0", "1.2.0"));
        assert!(!is_newer("1.1.9", "1.2.0"));
        assert!(!is_newer("1.3.0-beta", "1.2.0"));
        assert!(!is_newer("1.3", "1.2.0"));
    }

    #[test]
    fn package_names_match_what_mainstone_package_writes() {
        let name = package_name("1.3.0").unwrap();
        if cfg!(target_os = "macos") {
            assert_eq!(name, format!("mainstone-1.3.0-macos-{}.zip", std::env::consts::ARCH));
        } else if cfg!(windows) {
            assert_eq!(name, format!("mainstone-1.3.0-windows-{}.zip", std::env::consts::ARCH));
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(name, "mainstone-1.3.0-amd64.deb");
        }
    }

    /// Against the real latest release on GitHub: it is found, this
    /// platform's package is in it, and the download matches its digest.
    /// Run with `cargo test -- --ignored`.
    #[test]
    #[ignore = "downloads from GitHub"]
    fn the_latest_release_downloads_and_verifies() {
        let release = check_against("0.0.1", true).unwrap().expect("a newer release");
        let package = release.package.expect("a package for this platform with a digest");
        let dir = std::env::temp_dir().join(format!("mainstone-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(&package.name);
        download(&package, &file, &|_| {}).unwrap();

        // A tampered digest is refused.
        let mut wrong = package.clone();
        wrong.sha256 = "0".repeat(64);
        assert!(download(&wrong, &file, &|_| {}).is_err());

        #[cfg(target_os = "macos")]
        {
            // Swap a stand-in .app for the one in the download.
            let app = dir.join("Mainstone Cloud System.app");
            std::fs::create_dir_all(app.join("Contents")).unwrap();
            std::fs::write(app.join("Contents").join("old"), "old").unwrap();
            install_file(&file, &app, &dir).unwrap();
            assert!(app.join("Contents/MacOS/mainstone").exists());
            assert!(!app.join("Contents/old").exists());
            let leftovers: Vec<_> = std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with(".mainstone-"))
                .collect();
            assert!(leftovers.is_empty(), "{leftovers:?}");
            let verify = Command::new("codesign").args(["--verify", "--strict"]).arg(&app).status().unwrap();
            assert!(verify.success(), "the installed app's signature does not verify");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn digests_are_lowercase_hex() {
        assert_eq!(hex(&Sha256::digest(b"abc"))[..8], *"ba7816bf");
    }
}
