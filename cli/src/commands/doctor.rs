//! `trace doctor` — verify required external binaries.
//! Reports platform, lists found/missing binaries, prints per-platform
//! install instructions, exits 1 when a required one is missing. `mago`,
//! which reads `.sharp` files, is optional: it is listed with its install
//! command and never fails the check.

use crate::extraction::sharp::Mago;
use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

/// (name, darwin, linux, windows). List order is the order binaries are
/// probed and reported.
const REQUIRED_BINARIES: &[(&str, &str, &str, &str)] = &[
    (
        "ast-grep",
        "brew install ast-grep",
        "https://ast-grep.github.io/guide/quick-start.html",
        "scoop install ast-grep",
    ),
    (
        "scc",
        "brew install scc",
        "https://github.com/boyter/scc#installation",
        "scoop install scc",
    ),
    (
        "ctags",
        "brew install universal-ctags",
        "apt install universal-ctags  # or pacman -S ctags",
        "https://github.com/universal-ctags/ctags",
    ),
    (
        "git",
        "xcode-select --install",
        "apt install git  # or your package manager",
        "https://git-scm.com/download/win",
    ),
    (
        "rg",
        "brew install ripgrep",
        "apt install ripgrep  # or your package manager",
        "scoop install ripgrep",
    ),
];

/// How to install `mago`, which reads `.sharp` files: the shell installer
/// from php-sharp's GitHub releases, or the Composer package that puts it in
/// the project's `vendor/bin`.
const MAGO_INSTALL: &str = "curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/heyJordanParker/php-sharp/master/sharp/mago/scripts/install.sh | bash  # or composer require --dev heyjordanparker/mago-sharp";

const MAGO_UPDATE: &str = "tracer reads .sharp files with mago 0.8.1 or later, which has `mago outline`: mago self-update, or composer update heyjordanparker/mago-sharp";

/// Mirrors `deps.detect_platform`: darwin / linux / windows, else linux.
pub fn detect_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "windows",
        "linux" => "linux",
        _ => "linux",
    }
}

fn instruction_for(
    entry: &(&'static str, &'static str, &'static str, &'static str),
    platform: &str,
) -> &'static str {
    match platform {
        "darwin" => entry.1,
        "windows" => entry.3,
        _ => entry.2,
    }
}

/// `shutil.which(name)` equivalent — first PATH entry containing an
/// executable file named `name` (with `.exe`/`.cmd`/`.bat` on Windows).
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".to_string())
            .split(';')
            .map(|s| s.to_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path) {
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if is_executable_file(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable_file(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(p: &std::path::Path) -> bool {
    p.is_file()
}

/// Distinguish universal-ctags (required) from BSD ctags: run
/// `ctags --version` and check for the "Universal Ctags" marker. Probed
/// once per invocation — memoizing across invocations would have no
/// observable behavioral effect.
fn is_universal_ctags() -> bool {
    if which("ctags").is_none() {
        return false;
    }
    match crate::process::output(Command::new("ctags").arg("--version")) {
        Ok(out) => String::from_utf8_lossy(&out.stdout).contains("Universal Ctags"),
        Err(_) => false,
    }
}

/// Probe every required binary; returns (missing, present) names.
pub fn check_dependencies() -> (Vec<&'static str>, Vec<&'static str>) {
    let mut missing = Vec::new();
    let mut present = Vec::new();
    for entry in REQUIRED_BINARIES {
        let name = entry.0;
        let ok = if name == "ctags" {
            is_universal_ctags()
        } else {
            which(name).is_some()
        };
        if ok {
            present.push(name);
        } else {
            missing.push(name);
        }
    }
    (missing, present)
}

pub fn run() -> Result<()> {
    let (missing, present) = check_dependencies();
    let platform = detect_platform();
    println!("Platform: {platform}");
    println!();
    if !present.is_empty() {
        println!("Found:");
        for name in &present {
            println!("  \u{2713} {name}");
        }
        println!();
    }
    if !missing.is_empty() {
        println!("Missing:");
        for name in &missing {
            let entry = REQUIRED_BINARIES
                .iter()
                .find(|e| e.0 == *name)
                .expect("missing name is from REQUIRED_BINARIES");
            println!("  \u{2717} {name}");
            println!("    install: {}", instruction_for(entry, platform));
        }
        println!();
    }
    println!("Optional:");
    match crate::extraction::sharp::find(Path::new(".")) {
        Mago::Outlines(_) => println!("  \u{2713} mago, for .sharp files"),
        Mago::WithoutOutline(binary) => {
            println!("  \u{2717} mago, for .sharp files: {} has no outline command", binary.path());
            println!("    update: {MAGO_UPDATE}");
        }
        Mago::Missing => {
            println!("  \u{2717} mago, for .sharp files");
            println!("    install: {MAGO_INSTALL}");
        }
    }
    println!();
    if !missing.is_empty() {
        return Err(crate::process::fail(1, ""));
    }
    println!("All required binaries installed.");
    Ok(())
}
