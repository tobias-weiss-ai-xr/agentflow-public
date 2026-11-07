//! E2E guard for `af --version` — see
//! docs/superpowers/specs/2026-11-07-af-git-derived-build-version-design.md.
//!
//! Accepts both the plain `af 0.1.0` form (built outside git) and the
//! git-derived `af 0.1.0+<count>.<sha>[.dirty]` form:
//!   ^af [0-9]+\.[0-9]+\.[0-9]+(\+[0-9A-Za-z.-]+)?$

use agentflow::version::compose;

/// Validate one version string, returning Err with a message when malformed.
fn check_version_format(version: &str) -> Result<(), String> {
    let rest = version
        .strip_prefix("af ")
        .ok_or_else(|| format!("missing 'af ' prefix: {version:?}"))?;
    let (base, suffix) = match rest.split_once('+') {
        Some((b, s)) => (b, Some(s)),
        None => (rest, None),
    };
    let parts: Vec<&str> = base.split('.').collect();
    if parts.len() != 3
        || !parts
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(format!("bad base version: {base:?}"));
    }
    if let Some(suffix) = suffix {
        if suffix.is_empty()
            || suffix
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b == b'.' || b == b'-'))
        {
            return Err(format!("bad build metadata: {suffix:?}"));
        }
    }
    Ok(())
}

/// Run `af <flag>` and return the trimmed stdout on success.
fn af_version(flag: &str) -> String {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_af"))
        .arg(flag)
        .output()
        .expect("spawn af");
    assert!(out.status.success(), "af {flag} exited {}", out.status);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn version_flag_matches_build_version_format() {
    let version = af_version("--version");
    assert!(
        check_version_format(&version).is_ok(),
        "unexpected version: {}",
        check_version_format(&version).unwrap_err()
    );
}

#[test]
fn dash_v_matches_dash_dash_version() {
    assert_eq!(af_version("-V"), af_version("--version"));
}

/// Run a git command, returning trimmed stdout on success (mirrors build.rs).
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Recompute the version the build script should have emitted, mirroring
/// build.rs exactly: the same git facts fed to the same `compose()` rules
/// (which build.rs includes verbatim from src/version.rs). Falls back to the
/// plain crate version when git facts are unavailable.
fn git_derived_expected() -> String {
    let exact_tag = git(&["describe", "--tags", "--exact-match", "HEAD"]);
    let count = git(&["rev-list", "--count", "HEAD"]);
    let short_sha = git(&["rev-parse", "--short=7", "HEAD"]);
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let (Some(count), Some(short_sha)) = (count, short_sha) else {
        return env!("CARGO_PKG_VERSION").to_string();
    };
    compose(
        env!("CARGO_PKG_VERSION"),
        exact_tag.as_deref(),
        &count,
        &short_sha,
        dirty,
    )
}

#[test]
fn version_matches_live_git_facts() {
    let version = af_version("--version");
    let emitted = version
        .strip_prefix("af ")
        .expect("af --version must start with 'af '")
        .to_string();
    assert_eq!(
        emitted,
        git_derived_expected(),
        "af --version must match live git facts"
    );
}
