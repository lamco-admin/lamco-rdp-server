//! Build script for lamco-rdp-server
//!
//! Sets compile-time environment variables for build identification.

use std::process::Command;

fn main() {
    // Set build date
    let date_output = Command::new("date")
        .args(["+%Y-%m-%d"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=BUILD_DATE={}", date_output);

    // Set build time
    let time_output = Command::new("date")
        .args(["+%H:%M:%S"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "".to_string());
    println!("cargo:rustc-env=BUILD_TIME={}", time_output);

    // Set git commit hash
    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|_| "unknown".to_string());
    println!("cargo:rustc-env=GIT_HASH={}", git_hash);

    // Re-run if git HEAD changes
    println!("cargo:rerun-if-changed=.git/HEAD");
}
