// Pure version-composition rules for `af`.
//
// This file is the single source of truth for how the build-time version is
// assembled. `build.rs` includes it verbatim (`include!`) so the binary,
// the tests, and the build script share one implementation. Keep it
// dependency-free and `#[cfg(test)]`-safe: it is compiled both as part of
// the crate and inside the build script.
//
// NOTE: the comment header above deliberately uses plain `//` comments, not
// `//!` inner doc comments: when `build.rs` `include!`s this file verbatim,
// the `//!` lines would no longer sit at the start of the enclosing token
// stream and rustc would reject them (E0753). Module-level docs live on the
// `pub mod version;` declaration in `lib.rs` instead.

/// True when `tag` is `MAJOR.MINOR.PATCH` with an optional leading `v`
/// (e.g. `0.2.0` or `v0.2.0`). Anything else (e.g. `release-2026`) is not a
/// release tag.
fn is_semver_tag(tag: &str) -> bool {
    match tag
        .strip_prefix('v')
        .unwrap_or(tag)
        .split('.')
        .collect::<Vec<_>>()[..]
    {
        [maj, min, patch] => [maj, min, patch]
            .iter()
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())),
        _ => false,
    }
}

/// Compose the build version string printed by `af --version`.
///
/// Rule 1 — an exact semver release tag (`v0.2.0` / `0.2.0`) on a *clean*
/// tree prints the plain version (`0.2.0`), so tagged releases read naturally
/// and the `scripts/bump-version.sh` smoke test (`af --version` == `af <new>`)
/// keeps working.
///
/// Rule 2 — everything else prints `base+count.short_sha`, with `.dirty`
/// appended when the tree has uncommitted changes. The suffix is valid semver
/// build metadata, so the base version stays semver-correct.
pub fn compose(
    base: &str,
    exact_tag: Option<&str>,
    count: &str,
    short_sha: &str,
    dirty: bool,
) -> String {
    if let Some(tag) = exact_tag {
        if is_semver_tag(tag) && !dirty {
            return tag.strip_prefix('v').unwrap_or(tag).to_string();
        }
    }
    let mut version = format!("{base}+{count}.{short_sha}");
    if dirty {
        version.push_str(".dirty");
    }
    version
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_tag_prints_unique_metadata() {
        assert_eq!(
            compose("0.1.0", None, "42", "abc1234", false),
            "0.1.0+42.abc1234"
        );
    }

    #[test]
    fn plain_semver_tag_prints_clean_version() {
        assert_eq!(
            compose("0.1.0", Some("0.2.0"), "42", "abc1234", false),
            "0.2.0"
        );
    }

    #[test]
    fn v_prefixed_tag_strips_the_v() {
        assert_eq!(
            compose("0.1.0", Some("v0.2.0"), "42", "abc1234", false),
            "0.2.0"
        );
    }

    #[test]
    fn non_semver_tag_is_ignored() {
        assert_eq!(
            compose("0.1.0", Some("release-2026"), "42", "abc1234", false),
            "0.1.0+42.abc1234"
        );
    }

    #[test]
    fn dirty_at_tag_keeps_metadata() {
        assert_eq!(
            compose("0.1.0", Some("0.2.0"), "42", "abc1234", true),
            "0.1.0+42.abc1234.dirty"
        );
    }

    #[test]
    fn dirty_without_tag_appends_dirty() {
        assert_eq!(
            compose("0.1.0", None, "42", "abc1234", true),
            "0.1.0+42.abc1234.dirty"
        );
    }

    #[test]
    fn two_part_tag_is_not_semver() {
        assert_eq!(
            compose("0.1.0", Some("0.2"), "42", "abc1234", false),
            "0.1.0+42.abc1234"
        );
    }
}
