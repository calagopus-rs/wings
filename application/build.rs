use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    let is_git_repo = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);

    if is_git_repo {
        println!("cargo:rerun-if-changed=../.git/HEAD");

        if let Ok(head) = std::fs::read_to_string("../.git/HEAD") {
            if head.starts_with("ref: ") {
                let head_ref = head.trim_start_matches("ref: ").trim();
                println!("cargo:rerun-if-changed=../.git/{head_ref}");
            }
        }

        println!("cargo:rerun-if-changed=../.git/index");
    }

    let mut git_hash = "unknown".to_string();

    if is_git_repo {
        if let Ok(output) = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
        {
            if output.status.success() {
                if let Ok(hash) = String::from_utf8(output.stdout) {
                    git_hash = hash.trim().to_string();
                }
            }
        }
    }

    let target_arch =
        std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "unknown".to_string());
    let target_env =
        std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_else(|_| "unknown".to_string());

    println!("cargo:rustc-env=CARGO_GIT_COMMIT={git_hash}");
    println!("cargo:rustc-env=CARGO_TARGET={target_arch}-{target_env}");
}
