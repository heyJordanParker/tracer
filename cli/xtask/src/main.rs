use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const GLIBC: &str = "2.17";

const PLATFORMS: &[(&str, &str)] = &[
    ("darwin-arm64", "aarch64-apple-darwin"),
    ("darwin-x64", "x86_64-apple-darwin"),
    ("linux-arm64", "aarch64-unknown-linux-gnu"),
    ("linux-x64", "x86_64-unknown-linux-gnu"),
];

fn main() -> ExitCode {
    match std::env::args().nth(1).as_deref() {
        Some("dist") => match dist() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("xtask dist: {error}");
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("xtask: unknown task `{other}` (known: dist)");
            ExitCode::from(2)
        }
        None => {
            eprintln!("xtask: missing task (known: dist)");
            ExitCode::from(2)
        }
    }
}

fn dist() -> Result<(), String> {
    if std::env::consts::OS != "macos" {
        return Err(format!(
            "the macOS files build only on a Mac, and this is {}. Run cargo xtask dist on a Mac.",
            std::env::consts::OS
        ));
    }
    require_cross_toolchain()?;
    let root = crate_root();
    let target_dir = root.join(".target");
    let output = root.join("dist");
    let _ = fs::remove_dir_all(&output);
    fs::create_dir_all(&output).map_err(|error| format!("create {}: {error}", output.display()))?;

    let total = PLATFORMS.len();
    for (done, (platform, target)) in PLATFORMS.iter().enumerate() {
        println!("progress {done} {total} Building trace for {platform}");
        build(&root, &target_dir, target)?;
        let built = target_dir.join(target).join("release/trace");
        let file = output.join(format!("trace-{platform}"));
        fs::copy(&built, &file)
            .map_err(|error| format!("copy {} to {}: {error}", built.display(), file.display()))?;
    }
    println!("progress {total} {total} Built trace for every platform");
    Ok(())
}

fn build(root: &Path, target_dir: &Path, target: &str) -> Result<(), String> {
    let (subcommand, triple) = if target.contains("linux") {
        ("zigbuild", format!("{target}.{GLIBC}"))
    } else {
        ("build", target.to_string())
    };
    let status = rustup_cargo()
        .args([subcommand, "--release", "--locked", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .args(["--target", &triple, "--target-dir"])
        .arg(target_dir)
        .stdout(Stdio::null())
        .status()
        .map_err(|error| format!("run cargo {subcommand} for {target}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo {subcommand} failed for {target}"))
    }
}

fn require_cross_toolchain() -> Result<(), String> {
    let runs = |command: &mut Command| {
        command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let zig = runs(Command::new("zig").arg("version"));
    let zigbuild = runs(rustup_cargo().args(["zigbuild", "--help"]));
    if zig && zigbuild {
        return Ok(());
    }
    let mut missing = String::new();
    if !zig {
        missing.push_str("  ✗ zig\n    install: brew install zig\n");
    }
    if !zigbuild {
        missing.push_str("  ✗ cargo-zigbuild\n    install: cargo install cargo-zigbuild\n");
    }
    Err(format!(
        "the Linux files cross-compile with zig.\n\nMissing:\n{missing}\nThen run cargo xtask dist again."
    ))
}

fn rustup_cargo() -> Command {
    let mut command = Command::new("rustup");
    command.args(["run", "stable", "cargo"]);
    command
}

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the xtask package sits inside the trace crate")
        .to_path_buf()
}
