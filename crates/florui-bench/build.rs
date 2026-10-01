//! Records which commit a benchmark binary was built from, so a report names
//! the code that was measured and not whatever directory the run happened to
//! start in.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let commit = git(&["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());
    println!("cargo:rustc-env=FLORUI_BENCH_COMMIT={commit}");
    println!("cargo:rustc-env=FLORUI_BENCH_DIRTY={dirty}");

    // Rebuild when the checked-out commit moves: HEAD itself on a checkout,
    // and the branch it points at on a new commit.
    if let Some(head) = git(&["rev-parse", "--git-path", "HEAD"]) {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", &branch])
    {
        println!("cargo:rerun-if-changed={path}");
    }
}
