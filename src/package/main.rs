// Mainstone Cloud System — users, groups and devices in Microsoft Entra ID.
// Copyright (c) 2026 Will Richards. Released under the MIT licence; see LICENSE.

//! `mainstone-package`: builds the release package for the platform it runs on.
//!
//! ```text
//! cargo run --release --bin mainstone-package                # this machine's package
//! cargo run --release --bin mainstone-package -- --universal # macOS: Apple silicon and Intel
//! ```
//!
//! - macOS: `dist/Mainstone Cloud System.app`, signed ad hoc, and a zip of it.
//! - Linux: `dist/mainstone-<version>-<arch>.deb`, for Ubuntu and other Debian-based
//!   systems.
//! - Windows: `dist/mainstone-<version>-windows-<arch>.zip`.
//!
//! Each platform's packaging lives in its own module, compiled only on that
//! platform. Nothing here goes through a shell: the tools each one needs
//! (`codesign`, `dpkg-deb` and so on) are started directly, with their
//! arguments passed as they are.

#![cfg_attr(not(any(target_os = "macos", target_os = "linux", windows)), allow(dead_code))]

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod ubuntu;
#[cfg(windows)]
mod windows;

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

type Result<T = ()> = std::result::Result<T, String>;

/// The executable's name, as Cargo builds it.
const BIN: &str = "mainstone";
/// The name people see, from the window title to the applications menu.
#[cfg(target_os = "macos")]
const APP_NAME: &str = "Mainstone Cloud System";
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match package(&args) {
        Ok(written) => {
            println!("Wrote {}", written.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("mainstone-package: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_os = "macos")]
fn package(args: &[String]) -> Result<PathBuf> {
    macos::package(args)
}

#[cfg(target_os = "linux")]
fn package(args: &[String]) -> Result<PathBuf> {
    no_arguments(args)?;
    ubuntu::package()
}

#[cfg(windows)]
fn package(args: &[String]) -> Result<PathBuf> {
    no_arguments(args)?;
    windows::package()
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn package(_args: &[String]) -> Result<PathBuf> {
    Err("there is no package for this platform; use `cargo build --release`.".into())
}

/// For the platforms whose package takes no options; macOS reads its own.
#[cfg(any(target_os = "linux", windows))]
fn no_arguments(args: &[String]) -> Result {
    match args {
        [] => Ok(()),
        _ => Err(format!("unexpected arguments: {}", args.join(" "))),
    }
}

/// The checkout this was built from.
fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Cargo's output directory, wherever `CARGO_TARGET_DIR` has put it.
fn target_dir() -> PathBuf {
    std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .map(|dir| if dir.is_absolute() { dir } else { root().join(dir) })
        .unwrap_or_else(|| root().join("target"))
}

/// Where finished packages go.
fn dist() -> Result<PathBuf> {
    let dist = root().join("dist");
    std::fs::create_dir_all(&dist).map_err(|e| format!("could not create {}: {e}", dist.display()))?;
    Ok(dist)
}

/// `cargo build --release` of the app itself, with whatever else this
/// platform needs.
fn cargo_build(extra: &[&str], env: &[(&str, &str)]) -> Result {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    run(Command::new(cargo)
        .args(["build", "--release", "--locked", "--bin", BIN])
        .args(extra)
        .envs(env.iter().copied())
        .current_dir(root()))
}

/// Run a tool to completion, failing if it does.
fn run(command: &mut Command) -> Result {
    let shown = describe(command);
    println!("> {shown}");
    let status = command
        .status()
        .map_err(|e| format!("could not start {shown}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{shown} failed ({status})"))
    }
}

fn describe(command: &Command) -> String {
    std::iter::once(command.get_program())
        .chain(command.get_args())
        .map(|part| part.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// An empty directory at `path`, whatever was there before.
fn fresh_dir(path: &Path) -> Result {
    if path.exists() {
        std::fs::remove_dir_all(path).map_err(|e| format!("could not remove {}: {e}", path.display()))?;
    }
    std::fs::create_dir_all(path).map_err(|e| format!("could not create {}: {e}", path.display()))
}

/// Copy a file, creating the directories it goes into.
fn copy(from: &Path, to: &Path) -> Result {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::copy(from, to)
        .map(drop)
        .map_err(|e| format!("could not copy {} to {}: {e}", from.display(), to.display()))
}

#[cfg(unix)]
fn write(path: &Path, contents: &str) -> Result {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    std::fs::write(path, contents).map_err(|e| format!("could not write {}: {e}", path.display()))
}
