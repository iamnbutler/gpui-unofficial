//! Release version scheme for gpui-unofficial.
//!
//! Every published version is an upstream Zed version plus a *revision*
//! counter carried in the semver pre-release field:
//!
//! ```text
//!   Zed v1.16.0  ->  1.16.0-0   (first publish)
//!                    1.16.0-1   (our fix on top of the same upstream code)
//!   Zed v1.16.1  ->  1.16.1-0
//! ```
//!
//! We never publish a bare `1.16.0`. That is what makes the scheme work: a
//! pre-release sorts *below* the release of the same triple, so publishing
//! `1.16.0` first would leave us no room to ship a fix without colliding with
//! Zed's next tag. Starting at `-0` keeps the whole range `-1`, `-2`, ... above
//! whatever we already shipped, in order, forever.
//!
//! If upstream itself carries a pre-release (Zed's preview tags, `v1.16.0-pre`),
//! the revision is appended as a dot segment instead: `1.16.0-pre.0`.

use anyhow::{Context, Result, bail};
use std::process::Command;

/// An upstream version paired with our revision counter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseVersion {
    /// Upstream Zed version, without a leading `v` (e.g. `1.16.0`).
    pub upstream: String,
    /// Our revision on top of that upstream version. First publish is 0.
    pub revision: u32,
}

impl ReleaseVersion {
    pub fn new(upstream: impl Into<String>, revision: u32) -> Self {
        Self { upstream: upstream.into(), revision }
    }

    /// Parse a full release version (`1.16.0-0`, `v1.16.0-0`, `1.16.0-pre.0`).
    /// Returns `None` if the version carries no revision suffix.
    pub fn parse(version: &str) -> Option<Self> {
        let (upstream, revision) = split_revision(version);
        revision.map(|revision| Self { upstream, revision })
    }
}

impl std::fmt::Display for ReleaseVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // An upstream pre-release already occupies the `-` slot, so extend it
        // with a dot segment rather than starting a second pre-release.
        if self.upstream.contains('-') {
            write!(f, "{}.{}", self.upstream, self.revision)
        } else {
            write!(f, "{}-{}", self.upstream, self.revision)
        }
    }
}

/// Strip a leading `v` from a git tag.
pub fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

/// Split a version or tag into its upstream part and our revision, if any.
///
/// `1.16.0-0` -> (`1.16.0`, Some(0)); `1.16.0` -> (`1.16.0`, None);
/// `1.16.0-pre.0` -> (`1.16.0-pre`, Some(0)); `1.16.0-pre` -> (`1.16.0-pre`, None).
pub fn split_revision(version: &str) -> (String, Option<u32>) {
    let version = strip_v(version);
    let whole = || (version.to_string(), None);

    let Some(dash) = version.find('-') else {
        return whole();
    };
    let (base, pre) = (&version[..dash], &version[dash + 1..]);

    // The revision is always the last dot segment of the pre-release.
    let (upstream_pre, last) = match pre.rsplit_once('.') {
        Some((head, last)) => (Some(head), last),
        None => (None, pre),
    };

    // Numeric semver identifiers may not have leading zeros, so `00` is somebody
    // else's suffix, not one of ours.
    let numeric = !last.is_empty()
        && last.bytes().all(|b| b.is_ascii_digit())
        && (last.len() == 1 || !last.starts_with('0'));
    if !numeric {
        return whole();
    }
    let Ok(revision) = last.parse::<u32>() else {
        return whole();
    };

    let upstream = match upstream_pre {
        Some(head) => format!("{base}-{head}"),
        None => base.to_string(),
    };
    (upstream, Some(revision))
}

/// Every revision we have already published for `upstream`.
pub fn revisions_for(published: &[String], upstream: &str) -> Vec<u32> {
    let upstream = strip_v(upstream);
    let mut revisions: Vec<u32> = published
        .iter()
        .filter_map(|v| ReleaseVersion::parse(v))
        .filter(|v| v.upstream == upstream)
        .map(|v| v.revision)
        .collect();
    revisions.sort_unstable();
    revisions.dedup();
    revisions
}

/// The highest revision published for `upstream`, if any.
pub fn highest_revision(published: &[String], upstream: &str) -> Option<u32> {
    revisions_for(published, upstream).last().copied()
}

/// How to pick the revision for a release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionMode {
    /// Re-use the highest published revision so an interrupted release can be
    /// resumed. Falls back to 0 when nothing is published yet.
    Reuse,
    /// Allocate a fresh revision above everything published — a hotfix.
    Bump,
    /// Use exactly this revision.
    Exact(u32),
}

/// Resolve the version to release for `upstream`, given what is already on
/// crates.io.
pub fn resolve(upstream: &str, published: &[String], mode: RevisionMode) -> ReleaseVersion {
    let upstream = strip_v(upstream).to_string();
    let highest = highest_revision(published, &upstream);
    let revision = match mode {
        RevisionMode::Exact(revision) => revision,
        RevisionMode::Reuse => highest.unwrap_or(0),
        // Nothing published means there is nothing to fix: this is still the
        // first release, so --bump stays at 0.
        RevisionMode::Bump => highest.map_or(0, |highest| highest + 1),
    };
    ReleaseVersion::new(upstream, revision)
}

// ---------------------------------------------------------------------------
// crates.io index
// ---------------------------------------------------------------------------

/// Lists the versions a crate has published. Injectable so the resolution logic
/// above stays testable without network access.
pub trait VersionIndex {
    /// Non-yanked versions of `crate_name`, in index order. An unpublished
    /// crate yields an empty list rather than an error.
    fn published_versions(&self, crate_name: &str) -> Result<Vec<String>>;
}

/// The crates.io sparse index.
///
/// `cargo search` only reports a crate's *newest* version, which cannot answer
/// "which revisions of 1.16.0 exist?" — and its output is matched by substring,
/// so `1.16.0` spuriously matches `1.16.0-1`. The index gives us every version
/// exactly.
pub struct SparseIndex;

/// Path of a crate in the sparse index (`gp/ui/gpui-unofficial`).
pub fn index_path(crate_name: &str) -> String {
    let name = crate_name.to_lowercase();
    match name.len() {
        0 => name,
        1 => format!("1/{name}"),
        2 => format!("2/{name}"),
        3 => format!("3/{}/{name}", &name[0..1]),
        _ => format!("{}/{}/{name}", &name[0..2], &name[2..4]),
    }
}

impl VersionIndex for SparseIndex {
    fn published_versions(&self, crate_name: &str) -> Result<Vec<String>> {
        let url = format!("https://index.crates.io/{}", index_path(crate_name));

        let mut last_err = None;
        for attempt in 0..3u64 {
            if attempt > 0 {
                std::thread::sleep(std::time::Duration::from_secs(2 * attempt));
            }
            let output = match Command::new("curl")
                .args(["-sS", "--fail", "--location", "--max-time", "30", &url])
                .output()
                .with_context(|| format!("running curl for {url}"))
            {
                Ok(output) => output,
                Err(err) => {
                    last_err = Some(err);
                    continue;
                }
            };

            // 22 is curl's "HTTP error" exit code; for the index that means the
            // crate has never been published.
            if output.status.code() == Some(22) {
                return Ok(Vec::new());
            }
            if !output.status.success() {
                last_err = Some(anyhow::anyhow!(
                    "curl {url} failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
                continue;
            }

            return Ok(parse_index(&String::from_utf8_lossy(&output.stdout)));
        }

        bail!(last_err.unwrap_or_else(|| anyhow::anyhow!("could not read {url}")))
    }
}

/// Extract non-yanked versions from a sparse index document (one JSON object
/// per line).
pub fn parse_index(body: &str) -> Vec<String> {
    body.lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .filter(|entry| !entry["yanked"].as_bool().unwrap_or(false))
        .filter_map(|entry| entry["vers"].as_str().map(str::to_string))
        .collect()
}

/// Whether `crate_name` has published exactly `version`.
pub fn is_published(index: &dyn VersionIndex, crate_name: &str, version: &str) -> Result<bool> {
    let versions = index.published_versions(crate_name)?;
    Ok(versions.iter().any(|v| v == version))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn v(strings: &[&str]) -> Vec<String> {
        strings.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn formats_revision_as_pre_release() {
        assert_eq!(ReleaseVersion::new("1.16.0", 0).to_string(), "1.16.0-0");
        assert_eq!(ReleaseVersion::new("1.16.0", 12).to_string(), "1.16.0-12");
    }

    #[test]
    fn extends_an_upstream_pre_release_with_a_dot_segment() {
        // `1.16.0-pre-0` would be one pre-release identifier, not two, and
        // would sort next to `pre` rather than after it.
        assert_eq!(
            ReleaseVersion::new("1.16.0-pre", 1).to_string(),
            "1.16.0-pre.1"
        );
    }

    #[test]
    fn round_trips_through_parse() {
        for version in ["1.16.0-0", "1.16.0-7", "0.231.2-pre.0", "0.231.2-pre.3"] {
            let parsed = ReleaseVersion::parse(version).expect("has a revision");
            assert_eq!(parsed.to_string(), version);
        }
    }

    #[test]
    fn parses_leading_v() {
        assert_eq!(
            ReleaseVersion::parse("v1.16.0-2"),
            Some(ReleaseVersion::new("1.16.0", 2))
        );
    }

    #[test]
    fn bare_versions_have_no_revision() {
        assert_eq!(split_revision("1.16.0"), ("1.16.0".to_string(), None));
        assert_eq!(ReleaseVersion::parse("1.16.0"), None);
    }

    #[test]
    fn foreign_pre_release_suffixes_are_not_revisions() {
        // Upstream preview tags, and the shapes floated before we settled on
        // this scheme, must not be mistaken for our counter. `01` is not a
        // valid numeric semver identifier, so it is somebody else's suffix.
        for version in ["1.16.0-pre", "1.16.0-rc1", "1.16.0-01", "1.16.0-fix"] {
            assert_eq!(ReleaseVersion::parse(version), None, "{version}");
            assert_eq!(split_revision(version).0, version, "{version}");
        }
    }

    #[test]
    fn a_trailing_counter_belongs_to_the_pre_release_it_extends() {
        // `1.16.0-fix.1` is revision 1 of upstream `1.16.0-fix`, not of
        // `1.16.0` — the same rule that makes `1.16.0-pre.1` work. It therefore
        // never collides with the `1.16.0-N` line.
        assert_eq!(
            ReleaseVersion::parse("1.16.0-fix.1"),
            Some(ReleaseVersion::new("1.16.0-fix", 1))
        );
        assert!(revisions_for(&v(&["1.16.0-fix.1"]), "1.16.0").is_empty());
    }

    #[test]
    fn collects_revisions_for_one_upstream_only() {
        let published = v(&["1.15.0", "1.16.0-0", "1.16.0-2", "1.16.1-0", "1.16.0-1"]);
        assert_eq!(revisions_for(&published, "1.16.0"), vec![0, 1, 2]);
        assert_eq!(revisions_for(&published, "v1.16.0"), vec![0, 1, 2]);
        assert_eq!(highest_revision(&published, "1.16.0"), Some(2));
        assert_eq!(highest_revision(&published, "1.17.0"), None);
    }

    #[test]
    fn first_release_of_an_upstream_version_is_revision_zero() {
        let published = v(&["1.15.0", "1.16.0-0"]);
        for mode in [RevisionMode::Reuse, RevisionMode::Bump] {
            assert_eq!(
                resolve("v1.17.0", &published, mode).to_string(),
                "1.17.0-0",
                "{mode:?}"
            );
        }
    }

    #[test]
    fn reuse_resumes_an_interrupted_release() {
        // A publish that died halfway must be retried at the same version, so
        // the crates that made it through are skipped.
        let published = v(&["1.16.0-0"]);
        assert_eq!(
            resolve("1.16.0", &published, RevisionMode::Reuse).to_string(),
            "1.16.0-0"
        );
    }

    #[test]
    fn bump_allocates_the_next_revision() {
        let published = v(&["1.16.0-0", "1.16.0-1"]);
        assert_eq!(
            resolve("1.16.0", &published, RevisionMode::Bump).to_string(),
            "1.16.0-2"
        );
    }

    #[test]
    fn bump_clears_gaps_left_by_yanks() {
        // `1.16.0-1` was yanked, so it is absent from the index — but reusing
        // that number would fail to publish.
        let published = v(&["1.16.0-0", "1.16.0-2"]);
        assert_eq!(
            resolve("1.16.0", &published, RevisionMode::Bump).to_string(),
            "1.16.0-3"
        );
    }

    #[test]
    fn exact_revision_overrides_the_index() {
        let published = v(&["1.16.0-0", "1.16.0-1"]);
        assert_eq!(
            resolve("1.16.0", &published, RevisionMode::Exact(9)).to_string(),
            "1.16.0-9"
        );
    }

    #[test]
    fn upstream_pre_release_revisions_are_tracked_separately() {
        let published = v(&["1.16.0-pre.0", "1.16.0-0"]);
        assert_eq!(highest_revision(&published, "1.16.0-pre"), Some(0));
        assert_eq!(highest_revision(&published, "1.16.0"), Some(0));
        assert_eq!(
            resolve("v1.16.0-pre", &published, RevisionMode::Bump).to_string(),
            "1.16.0-pre.1"
        );
    }

    #[test]
    fn index_paths_follow_the_crates_io_layout() {
        assert_eq!(index_path("a"), "1/a");
        assert_eq!(index_path("ab"), "2/ab");
        assert_eq!(index_path("abc"), "3/a/abc");
        assert_eq!(index_path("gpui-unofficial"), "gp/ui/gpui-unofficial");
        assert_eq!(
            index_path("gpui-macros-gpui-unofficial"),
            "gp/ui/gpui-macros-gpui-unofficial"
        );
    }

    #[test]
    fn index_parsing_skips_yanked_versions() {
        let body = r#"{"name":"gpui-unofficial","vers":"1.15.0","yanked":false}
{"name":"gpui-unofficial","vers":"1.16.0-0","yanked":true}
{"name":"gpui-unofficial","vers":"1.16.0-1","yanked":false}
"#;
        assert_eq!(parse_index(body), v(&["1.15.0", "1.16.0-1"]));
    }

    #[test]
    fn index_parsing_ignores_malformed_lines() {
        let body = "not json\n{\"vers\":\"1.16.0-0\",\"yanked\":false}\n\n";
        assert_eq!(parse_index(body), v(&["1.16.0-0"]));
    }

    // --- exact publication checks ------------------------------------------

    struct FakeIndex(Vec<String>);

    impl VersionIndex for FakeIndex {
        fn published_versions(&self, _crate_name: &str) -> Result<Vec<String>> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn publication_check_is_exact_not_substring() {
        let index = FakeIndex(v(&["1.16.0-1"]));
        assert!(is_published(&index, "gpui-unofficial", "1.16.0-1").unwrap());
        // The old `cargo search` substring check said yes to both of these.
        assert!(!is_published(&index, "gpui-unofficial", "1.16.0").unwrap());
        assert!(!is_published(&index, "gpui-unofficial", "1.16.0-").unwrap());
    }
}
