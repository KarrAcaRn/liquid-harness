//! Compile-time build facts for `lh doctor` / `/lh/v1/self`: git commit, dirty
//! flag and build time. Git runs only here, never at runtime; without git (a
//! source tarball) the values are "unknown". No dependencies.
use std::{path::Path, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = match git(&["status", "--porcelain", "--untracked-files=no"]) {
        Some(status) => (!status.is_empty()).to_string(),
        None => "unknown".into(),
    };
    let built = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    println!("cargo:rustc-env=LH_BUILD_COMMIT={commit}");
    println!("cargo:rustc-env=LH_BUILD_DIRTY={dirty}");
    println!("cargo:rustc-env=LH_BUILD_UNIX_TIME={built}");
    // Rerun when the checkout moves (commit, checkout, staging) or any
    // workspace source changes, so the dirty flag follows edits in every crate.
    if let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) {
        for name in ["HEAD", "index", "logs/HEAD"] {
            let path = Path::new(&dir).join(name);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for name in ["crates", "plugins", "Cargo.toml", "Cargo.lock"] {
        let path = root.join(name);
        if path.exists() {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
