//! Windows: `mainstone-<version>-windows-<arch>.zip`, holding `mainstone.exe` and the
//! licences.

use std::path::PathBuf;
use std::process::Command;

use crate::{BIN, Result, VERSION, cargo_build, copy, dist, fresh_dir, root, run, target_dir};

pub fn package() -> Result<PathBuf> {
    cargo_build(&[], &[])?;

    let folder = format!("{BIN}-{VERSION}");
    let staging = target_dir().join("zip");
    let stage = staging.join(&folder);
    fresh_dir(&stage)?;
    let exe = format!("{BIN}.exe");
    copy(&target_dir().join("release").join(&exe), &stage.join(&exe))?;
    for file in ["LICENSE", "README.md", "assets/fonts/UBUNTU-FONT-LICENCE-1.0.txt"] {
        let from = root().join(file);
        let name = from.file_name().expect("a file name").to_owned();
        copy(&from, &stage.join(name))?;
    }

    let zip = dist()?.join(format!(
        "{BIN}-{VERSION}-windows-{}.zip",
        std::env::consts::ARCH
    ));
    if zip.exists() {
        std::fs::remove_file(&zip).map_err(|e| e.to_string())?;
    }
    // Windows' own tar (bsdtar, in System32 since Windows 10) writes zips. It
    // is named in full so that another tar earlier on the PATH, such as Git's,
    // which cannot, is never picked up instead.
    let system_root = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
    let tar = PathBuf::from(system_root).join("System32").join("tar.exe");
    run(Command::new(tar)
        .args(["-a", "-c", "-f"])
        .arg(&zip)
        .arg("-C")
        .arg(&staging)
        .arg(&folder))?;
    Ok(zip)
}
