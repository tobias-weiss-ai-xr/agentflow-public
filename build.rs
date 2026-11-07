//! Build script: derive a unique, human-readable `af` version from git at
//! build time and hand it to the crate as `AF_BUILD_VERSION`.
//!
//! Rules live in `src/version.rs` (included verbatim below so there is exactly
//! one implementation, unit-tested in the crate). Any git failure falls back
//! to the plain crate version; the build never fails because of git.

// Single source of truth for the version rules (see src/version.rs).
include!(concat!(env!("CARGO_MANIFEST_DIR"), "/src/version.rs"));

use std::process::Command;

/// Run a git command, returning trimmed stdout on success.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let base = env!("CARGO_PKG_VERSION");

    // Re-run this script when the git state may have changed (new commit,
    // checkout, `git add`), so a rebuild recomputes the version instead of
    // Cargo serving a cached one.
    let git_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".git");
    if git_path.is_dir() {
        // Normal clone: watch the whole git dir.
        println!("cargo:rerun-if-changed={}", git_path.display());
    } else if git_path.is_file() {
        // Git worktree: .git is a pointer file; watches the worktree's HEAD.
        println!("cargo:rerun-if-changed={}", git_path.join("HEAD").display());
    }

    let exact_tag = git(&["describe", "--tags", "--exact-match", "HEAD"]);
    let count = git(&["rev-list", "--count", "HEAD"]);
    let short_sha = git(&["rev-parse", "--short=7", "HEAD"]);
    let dirty = git(&["status", "--porcelain"]).is_some_and(|status| !status.is_empty());

    let (Some(count), Some(short_sha)) = (count, short_sha) else {
        // No commits or git unavailable: fall back to the plain crate version.
        println!("cargo:rustc-env=AF_BUILD_VERSION={base}");
        println!("cargo:warning=af: could not determine git version, using {base}");
        return;
    };

    let version = compose(base, exact_tag.as_deref(), &count, &short_sha, dirty);
    println!("cargo:rustc-env=AF_BUILD_VERSION={version}");
}
