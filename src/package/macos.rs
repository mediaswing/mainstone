//! macOS: `Mainstone Cloud System.app`, signed ad hoc, and a zip of it.
//!
//! The signature is ad hoc (`codesign --sign -`), which is enough for the app
//! to run on Apple silicon. It is not notarised, so the first time a
//! downloaded copy is opened, it has to be allowed under System Settings >
//! Privacy & Security. An `AppIcon.icns` in `packaging/macos/` becomes the
//! bundle's icon.

use std::path::PathBuf;
use std::process::Command;

use crate::{APP_NAME, BIN, Result, VERSION, cargo_build, copy, dist, fresh_dir, root, run, target_dir, write};

const IDENTIFIER: &str = "io.github.mediaswing.mainstone";
/// The oldest macOS the binary is built for, and the one Info.plist states.
const MINIMUM_MACOS: &str = "11.0";
const TARGETS: [&str; 2] = ["aarch64-apple-darwin", "x86_64-apple-darwin"];

pub fn package(args: &[String]) -> Result<PathBuf> {
    let universal = match args {
        [] => false,
        [flag] if flag == "--universal" => true,
        _ => return Err("usage: mainstone-package [--universal]".into()),
    };
    let env = [("MACOSX_DEPLOYMENT_TARGET", MINIMUM_MACOS)];

    let (executable, arch) = if universal {
        // Not every toolchain comes from rustup; one that does not has to
        // have both targets installed already.
        let _ = Command::new("rustup").args(["target", "add"]).args(TARGETS).status();
        let mut built = Vec::new();
        for target in TARGETS {
            cargo_build(&["--target", target], &env)?;
            built.push(target_dir().join(target).join("release").join(BIN));
        }
        let fat = target_dir().join("universal");
        std::fs::create_dir_all(&fat).map_err(|e| e.to_string())?;
        let executable = fat.join(BIN);
        run(Command::new("lipo")
            .arg("-create")
            .arg("-output")
            .arg(&executable)
            .args(&built))?;
        (executable, "universal")
    } else {
        cargo_build(&[], &env)?;
        (target_dir().join("release").join(BIN), std::env::consts::ARCH)
    };

    let dist = dist()?;
    let app = dist.join(format!("{APP_NAME}.app"));
    let contents = app.join("Contents");
    fresh_dir(&app)?;
    copy(&executable, &contents.join("MacOS").join(BIN))?;
    copy(&root().join("LICENSE"), &contents.join("Resources").join("LICENSE"))?;

    let icon = root().join("packaging").join("macos").join("AppIcon.icns");
    let icon_entry = if icon.exists() {
        copy(&icon, &contents.join("Resources").join("AppIcon.icns"))?;
        "\n    <key>CFBundleIconFile</key>\n    <string>AppIcon</string>"
    } else {
        ""
    };
    let plist = contents.join("Info.plist");
    write(&plist, &info_plist(icon_entry))?;
    run(Command::new("plutil").arg("-lint").arg(&plist))?;

    // Ad hoc, with the hardened runtime: nothing in the app needs an
    // exception to it.
    run(Command::new("codesign")
        .args(["--force", "--options", "runtime", "--timestamp=none", "--sign", "-"])
        .arg(&app))?;
    run(Command::new("codesign")
        .args(["--verify", "--strict", "--verbose=2"])
        .arg(&app))?;

    // A copy of the signed executable for virus scanners, under a name that
    // says which platform it is for. Inside the bundle it is plain `mainstone`,
    // which reads as the same file as Windows' `mainstone.exe` in a list of
    // reports. Taken after signing, so it is byte for byte what people run.
    copy(&contents.join("MacOS").join(BIN), &target_dir().join(format!("{BIN}-macos")))?;

    let zip = dist.join(format!("{BIN}-{VERSION}-macos-{arch}.zip"));
    if zip.exists() {
        std::fs::remove_file(&zip).map_err(|e| e.to_string())?;
    }
    // ditto keeps the bundle's symlinks, permissions and signature intact,
    // which a plain zip does not.
    run(Command::new("ditto")
        .args(["-c", "-k", "--sequesterRsrc", "--keepParent"])
        .arg(&app)
        .arg(&zip))?;
    println!("Wrote {}", app.display());
    Ok(zip)
}

fn info_plist(icon_entry: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleName</key>
    <string>{APP_NAME}</string>
    <key>CFBundleExecutable</key>
    <string>{BIN}</string>
    <key>CFBundleIdentifier</key>
    <string>{IDENTIFIER}</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>{VERSION}</string>
    <key>CFBundleVersion</key>
    <string>{VERSION}</string>{icon_entry}
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.utilities</string>
    <key>LSMinimumSystemVersion</key>
    <string>{MINIMUM_MACOS}</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>Copyright (c) 2026 Will Richards. MIT licence.</string>
</dict>
</plist>
"#
    )
}
