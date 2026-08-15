#![forbid(unsafe_code)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

fn main() -> Result<()> {
    let task = env::args().nth(1).unwrap_or_else(|| "help".to_owned());
    let root = workspace_root()?;

    match task.as_str() {
        "check" => check(&root),
        "fmt" => cargo(&root, &["fmt", "--all"]),
        "fmt-check" => cargo(&root, &["fmt", "--all", "--", "--check"]),
        "build" => cargo(&root, &["check", "--workspace", "--all-targets"]),
        "lint" => cargo(
            &root,
            &[
                "clippy",
                "--workspace",
                "--all-targets",
                "--all-features",
                "--",
                "-D",
                "warnings",
            ],
        ),
        "test" => cargo(&root, &["test", "--workspace", "--all-features"]),
        "doc" => cargo(
            &root,
            &[
                "doc",
                "--workspace",
                "--all-features",
                "--no-deps",
            ],
        ),
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        other => bail!("unknown xtask command {other:?}"),
    }
}

fn check(root: &Path) -> Result<()> {
    cargo(root, &["fmt", "--all", "--", "--check"])?;
    cargo(root, &["check", "--workspace", "--all-targets"])?;
    cargo(
        root,
        &[
            "clippy",
            "--workspace",
            "--all-targets",
            "--all-features",
            "--",
            "-D",
            "warnings",
        ],
    )?;
    cargo(root, &["test", "--workspace", "--all-features"])?;
    cargo(
        root,
        &[
            "doc",
            "--workspace",
            "--all-features",
            "--no-deps",
        ],
    )?;
    Ok(())
}

fn cargo(root: &Path, arguments: &[&str]) -> Result<()> {
    eprintln!("+ cargo {}", arguments.join(" "));
    let status = Command::new("cargo")
        .args(arguments)
        .current_dir(root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("failed to start cargo {}", arguments.join(" ")))?;

    if !status.success() {
        bail!("cargo {} failed with {status}", arguments.join(" "));
    }
    Ok(())
}

fn workspace_root() -> Result<PathBuf> {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .map(Path::to_path_buf)
        .context("xtask manifest directory has no parent")
}

fn print_help() {
    println!("Exbawks repository tasks");
    println!();
    println!("  cargo xtask check      Run all required checks.");
    println!("  cargo xtask fmt        Format the workspace.");
    println!("  cargo xtask fmt-check  Check formatting.");
    println!("  cargo xtask build      Check all workspace targets.");
    println!("  cargo xtask lint       Run Clippy with warnings denied.");
    println!("  cargo xtask test       Run all tests.");
    println!("  cargo xtask doc        Build workspace documentation.");
}
