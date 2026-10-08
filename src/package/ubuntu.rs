//! Linux: `mainstone-<version>-<arch>.deb`, for Ubuntu and other Debian-based
//! systems.
//!
//! It installs `/usr/bin/mainstone` and a "Mainstone Cloud System" entry in the
//! applications menu. Building it needs `dpkg-dev` (`sudo apt install
//! dpkg-dev`). A 256×256 `mainstone.png` in `packaging/ubuntu/` becomes the menu
//! entry's icon.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{BIN, Result, VERSION, cargo_build, copy, describe, dist, fresh_dir, root, run, target_dir, write};

const MAINTAINER: &str = "Will Richards <mediaswing@outlook.com>";

/// The applications-menu entry, installed as
/// `/usr/share/applications/mainstone.desktop` (the freedesktop.org desktop entry
/// format). `StartupWMClass` matches the app ID set in `main.rs`, so the
/// running window is grouped under this entry in the dock.
const DESKTOP_ENTRY: &str = "[Desktop Entry]
Type=Application
Name=Mainstone Cloud System
GenericName=Entra ID and Intune Manager
Comment=Manage users, groups and devices in Microsoft Entra ID and Intune
Exec=mainstone
Icon=mainstone
Terminal=false
Categories=System;Network;
Keywords=Entra;Azure;Intune;Microsoft;users;groups;devices;
StartupWMClass=mainstone
";

/// winit and glow open the window-system and GL libraries at run time rather
/// than linking them, so `dpkg-shlibdeps` cannot see them. The keyboard and
/// GL libraries are needed whatever the session.
const RUNTIME_DEPENDS: &str = "libxkbcommon0, libxkbcommon-x11-0, libegl1 | libgl1";
/// Wayland's and X11's own libraries are only needed for the session in use,
/// and every desktop install has them. The portal is what the file dialogs
/// talk to.
const RECOMMENDS: &str =
    "libwayland-client0, libwayland-cursor0, libx11-6, libxcursor1, libxi6, xdg-desktop-portal";

pub fn package() -> Result<PathBuf> {
    let arch = output(Command::new("dpkg-architecture").arg("-qDEB_HOST_ARCH"))?
        .trim()
        .to_owned();
    cargo_build(&[], &[])?;

    let stage = target_dir().join("deb").join(format!("{BIN}_{VERSION}_{arch}"));
    fresh_dir(&stage)?;
    let packaging = root().join("packaging").join("ubuntu");
    let usr = stage.join("usr");
    let binary = usr.join("bin").join(BIN);
    install(&target_dir().join("release").join(BIN), &binary, 0o755)?;
    let desktop = usr.join("share/applications").join(format!("{BIN}.desktop"));
    write(&desktop, DESKTOP_ENTRY)?;
    set_mode(&desktop, 0o644)?;
    let doc = usr.join("share/doc").join(BIN);
    install(&root().join("LICENSE"), &doc.join("copyright"), 0o644)?;
    install(
        &root().join("assets/fonts/UBUNTU-FONT-LICENCE-1.0.txt"),
        &doc.join("UBUNTU-FONT-LICENCE-1.0.txt"),
        0o644,
    )?;
    let icon = packaging.join("mainstone.png");
    if icon.exists() {
        install(&icon, &usr.join("share/icons/hicolor/256x256/apps").join(format!("{BIN}.png")), 0o644)?;
    }

    let linked = linked_libraries(&binary)?;
    let depends = if linked.is_empty() {
        RUNTIME_DEPENDS.to_owned()
    } else {
        format!("{linked}, {RUNTIME_DEPENDS}")
    };
    let installed_kib = size_of(&stage)?.div_ceil(1024);

    let control_dir = stage.join("DEBIAN");
    std::fs::create_dir_all(&control_dir).map_err(|e| e.to_string())?;
    set_mode(&control_dir, 0o755)?;
    let control = control_dir.join("control");
    write(
        &control,
        &format!(
            "Package: {BIN}
Version: {VERSION}
Section: admin
Priority: optional
Architecture: {arch}
Maintainer: {MAINTAINER}
Installed-Size: {installed_kib}
Depends: {depends}
Recommends: {RECOMMENDS}
Homepage: https://github.com/mediaswing/mainstone
Description: Mainstone Cloud System for Microsoft Entra ID and Intune
 A desktop app for users, groups and devices in Microsoft Entra ID, with
 Intune's remote actions, CSV import and export, and a copy of the
 directory to MariaDB.
"
        ),
    )?;
    set_mode(&control, 0o644)?;

    let deb = dist()?.join(format!("{BIN}-{VERSION}-{arch}.deb"));
    run(Command::new("dpkg-deb")
        .args(["--build", "--root-owner-group"])
        .arg(&stage)
        .arg(&deb))?;
    run(Command::new("dpkg-deb").arg("--info").arg(&deb))?;
    Ok(deb)
}

/// The packages providing the shared libraries the binary links against, as
/// dpkg itself works them out.
fn linked_libraries(binary: &Path) -> Result<String> {
    // dpkg-shlibdeps insists on a debian/control in the directory it runs
    // from, so it gets a throwaway one.
    let work = target_dir().join("deb").join("shlibdeps");
    fresh_dir(&work)?;
    write(
        &work.join("debian").join("control"),
        &format!("Source: {BIN}\n\nPackage: {BIN}\nArchitecture: any\n"),
    )?;
    let found = output(Command::new("dpkg-shlibdeps").arg("-O").arg(binary).current_dir(&work))?;
    Ok(found
        .lines()
        .find_map(|line| line.strip_prefix("shlibs:Depends="))
        .unwrap_or_default()
        .trim()
        .to_owned())
}

/// What a tool printed, failing if it failed.
fn output(command: &mut Command) -> Result<String> {
    let shown = describe(command);
    let out = command.output().map_err(|e| {
        format!("could not start {shown}: {e}. Building a .deb needs dpkg-dev: sudo apt install dpkg-dev")
    })?;
    if !out.status.success() {
        return Err(format!(
            "{shown} failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{shown} printed something unreadable: {e}"))
}

/// Copy a file into the package with the mode it should be installed with.
fn install(from: &Path, to: &Path, mode: u32) -> Result {
    copy(from, to)?;
    set_mode(to, mode)
}

fn set_mode(path: &Path, mode: u32) -> Result {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
        .map_err(|e| format!("could not set the mode of {}: {e}", path.display()))
}

/// Bytes in every file under `dir`, for `Installed-Size`.
fn size_of(dir: &Path) -> Result<u64> {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let meta = entry.metadata().map_err(|e| e.to_string())?;
        total += if meta.is_dir() { size_of(&entry.path())? } else { meta.len() };
    }
    Ok(total)
}
