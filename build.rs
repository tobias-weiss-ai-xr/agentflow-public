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
    //
    // We watch the HEAD and index FILES, not the `.git` DIRECTORY. Watching
    // the whole directory made Cargo re-run this script on every invocation:
    // git operations during the build (in particular the transient
    // `.git/index.lock` created by our own `git status` call below) bump the
    // directory's mtime, Cargo notices, re-runs the script, and the re-run
    // rewrites this script's output file — so the crate was always considered
    // dirty and rebuilt from scratch on every cargo build/test/clippy.
    //
    // Watching the specific files captures exactly the state that determines
    // the version: a commit rewrites `.git/index` (count + short-sha change),
    // a checkout moves `.git/HEAD`, and staging touches `.git/index` (dirty
    // flag changes). Our own (read-only) `git status --porcelain` does not
    // touch those files, so there is no churn. Accepted minor limitations: a
    // plain edit to a tracked file touches neither `.git/HEAD` nor
    // `.git/index`, so the `.dirty` suffix can go stale until the next
    // `git add`; and ref-only operations that touch neither file (e.g.
    // `git reset --soft`, `git branch -f`, or fetching a new tag onto the
    // current HEAD) can leave the version stale until the next
    // commit/checkout/stage.
    let git_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".git");
    if git_path.is_dir() {
        // Normal clone: watch the specific git files whose changes matter.
        let head = git_path.join("HEAD");
        if head.exists() {
            println!("cargo:rerun-if-changed={}", head.display());
        }
        let index = git_path.join("index");
        if index.exists() {
            println!("cargo:rerun-if-changed={}", index.display());
        }
    } else if git_path.is_file() {
        // Linked worktree or submodule: `.git` is a file whose content is
        // `gitdir: <path>\n`, a pointer to the real git dir. Traversing
        // `<manifest>/.git/HEAD` through that regular file can never exist,
        // so parse the pointer, resolve it relative to the manifest dir if it
        // is not absolute, and watch the real HEAD and index files instead.
        // If parsing fails, emit nothing and let Cargo fall back to its
        // default whole-package tracking (same as today's behavior).
        if let Ok(pointer) = std::fs::read_to_string(&git_path) {
            let gitdir = pointer
                .lines()
                .next()
                .unwrap_or_default()
                .strip_prefix("gitdir:")
                .map(str::trim)
                .map(std::path::Path::new);
            if let Some(gitdir) = gitdir {
                let resolved = if gitdir.is_absolute() {
                    gitdir.to_path_buf()
                } else {
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(gitdir)
                };
                let head = resolved.join("HEAD");
                if head.exists() {
                    println!("cargo:rerun-if-changed={}", head.display());
                }
                let index = resolved.join("index");
                if index.exists() {
                    println!("cargo:rerun-if-changed={}", index.display());
                }
            }
        }
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
