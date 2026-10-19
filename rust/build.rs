//! Build script for CodexBar
//!
//! Injects build metadata (git commit) at compile time.

use std::process::Command;

fn git_value(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        })
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn watch_git_path(name: &str) -> Option<std::path::PathBuf> {
    let path = std::path::PathBuf::from(git_value(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-path",
        name,
    ])?);
    // Missing rerun paths make Cargo rerun this script on every invocation.
    if !path.is_file() {
        return None;
    }
    println!("cargo:rerun-if-changed={}", path.display());
    Some(path)
}

fn main() {
    let git_commit =
        git_value(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());

    println!("cargo:rustc-env=GIT_COMMIT={}", git_commit);

    println!("cargo:rerun-if-changed=build.rs");
    // Git resolves metadata correctly for clones, worktrees and submodules.
    if let Some(head_path) = watch_git_path("HEAD")
        && let Ok(head) = std::fs::read_to_string(head_path)
        && let Some(reference) = head.trim().strip_prefix("ref: ")
    {
        drop(watch_git_path(reference));
    }
    drop(watch_git_path("packed-refs"));
}
