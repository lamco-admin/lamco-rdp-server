//! Build script for lamco-rdp-server
//!
//! Sets compile-time environment variables for build identification.

use std::process::Command;

fn run_command(program: &str, args: &[&str], fallback: &str) -> String {
    match Command::new(program).args(args).output() {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => fallback.to_string(),
    }
}

/// Reads the resolved version of a dependency straight from Cargo.lock, so
/// diagnostics report what this build actually linked rather than the
/// (possibly range-based) requirement in Cargo.toml.
fn cargo_lock_pkg_version(name: &str) -> String {
    let Ok(lock) = std::fs::read_to_string("Cargo.lock") else {
        return "unknown".to_string();
    };
    let needle = format!("name = \"{name}\"");
    lock.split("[[package]]")
        .find(|block| block.contains(&needle))
        .and_then(|block| {
            block
                .lines()
                .find_map(|line| line.trim().strip_prefix("version = \"")?.strip_suffix('"'))
        })
        .unwrap_or("unknown")
        .to_string()
}

fn main() {
    let date_output = run_command("date", &["+%Y-%m-%d"], "unknown");
    println!("cargo:rustc-env=BUILD_DATE={date_output}");

    let time_output = run_command("date", &["+%H:%M:%S"], "");
    println!("cargo:rustc-env=BUILD_TIME={time_output}");

    let git_hash = run_command("git", &["rev-parse", "--short", "HEAD"], "unknown");
    println!("cargo:rustc-env=GIT_HASH={git_hash}");

    let pipewire_crate_version = cargo_lock_pkg_version("lamco-pipewire");
    println!("cargo:rustc-env=LAMCO_PIPEWIRE_VERSION={pipewire_crate_version}");
    println!("cargo:rerun-if-changed=Cargo.lock");

    // Re-run when HEAD or the resolved ref target changes.
    //
    // .git/HEAD is a symbolic ref on a branch (e.g. "ref: refs/heads/main") and
    // its file contents do not change between commits — only the resolved target
    // (.git/refs/heads/<branch> for loose refs, .git/packed-refs after gc) does.
    // Watching only .git/HEAD leaves GIT_HASH stale across same-branch commits.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    if let Ok(head) = std::fs::read_to_string(".git/HEAD")
        && let Some(ref_path) = head.trim().strip_prefix("ref: ")
    {
        println!("cargo:rerun-if-changed=.git/{ref_path}");
    }
}
