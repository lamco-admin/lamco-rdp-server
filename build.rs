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

fn main() {
    let date_output = run_command("date", &["+%Y-%m-%d"], "unknown");
    println!("cargo:rustc-env=BUILD_DATE={date_output}");

    let time_output = run_command("date", &["+%H:%M:%S"], "");
    println!("cargo:rustc-env=BUILD_TIME={time_output}");

    let git_hash = run_command("git", &["rev-parse", "--short", "HEAD"], "unknown");
    println!("cargo:rustc-env=GIT_HASH={git_hash}");

    // Re-run if git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
}
