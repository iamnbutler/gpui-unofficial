use anyhow::Result;
use std::process::Command;

use crate::transform::{crate_name_from_path, unofficial_name, CRATE_PUBLISH_ORDER};
use crate::version::{self, RevisionMode, SparseIndex, VersionIndex};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct VerifyReport {
    pub release_branch_exists: bool,
    pub github_release_exists: bool,
    pub crates_published: Vec<CrateStatus>,
}

#[derive(Debug, PartialEq)]
pub struct CrateStatus {
    pub name: String,
    pub version: String,
    pub published: bool,
}

impl VerifyReport {
    pub fn is_complete(&self) -> bool {
        self.release_branch_exists
            && self.github_release_exists
            && self.crates_published.iter().all(|c| c.published)
    }

    pub fn missing_crates(&self) -> Vec<&CrateStatus> {
        self.crates_published.iter().filter(|c| !c.published).collect()
    }

    pub fn print_summary(&self) {
        println!(
            "  Release branch : {}",
            status_icon(self.release_branch_exists)
        );
        println!(
            "  GitHub release : {}",
            status_icon(self.github_release_exists)
        );

        let total = self.crates_published.len();
        let done = self.crates_published.iter().filter(|c| c.published).count();
        println!("  Crates on crates.io: {done}/{total}");

        for c in self.missing_crates() {
            println!("    ✗ {}@{}", c.name, c.version);
        }
    }
}

fn status_icon(ok: bool) -> &'static str {
    if ok { "✓" } else { "✗" }
}

// ---------------------------------------------------------------------------
// Checker trait — injectable for tests
// ---------------------------------------------------------------------------

/// Abstraction over all external I/O so the core logic is testable without
/// spawning real processes or hitting the network.
pub trait ReleaseChecker {
    /// Returns true if `release/<version>` exists as a remote branch.
    fn release_branch_exists(&self, repo_url: &str, version: &str) -> bool;

    /// Returns true if a GitHub release for `tag` exists in `repo`
    /// (owner/name format, e.g. "iamnbutler/gpui-unofficial").
    fn github_release_exists(&self, repo: &str, tag: &str) -> bool;

    /// Returns true if `name@version` is visible on the crates.io sparse index.
    fn crate_version_published(&self, name: &str, version: &str) -> bool;
}

// ---------------------------------------------------------------------------
// Real implementation — spawns processes
// ---------------------------------------------------------------------------

pub struct LiveChecker;

impl ReleaseChecker for LiveChecker {
    fn release_branch_exists(&self, repo_url: &str, version: &str) -> bool {
        let branch = format!("release/{}", version.trim_start_matches('v'));
        Command::new("git")
            .args(["ls-remote", "--heads", repo_url, &branch])
            .output()
            .ok()
            .is_some_and(|o| !o.stdout.is_empty())
    }

    fn github_release_exists(&self, repo: &str, tag: &str) -> bool {
        // Try gh CLI first (works without auth for public repos)
        let gh_ok = Command::new("gh")
            .args(["release", "view", tag, "--repo", repo])
            .output()
            .ok()
            .is_some_and(|o| o.status.success());

        if gh_ok {
            return true;
        }

        // Fallback: check for the git tag on the remote
        Command::new("git")
            .args([
                "ls-remote",
                "--tags",
                &format!("https://github.com/{repo}"),
                tag,
            ])
            .output()
            .ok()
            .is_some_and(|o| !o.stdout.is_empty())
    }

    fn crate_version_published(&self, name: &str, version: &str) -> bool {
        // Exact match against the index. `cargo search` only reports a crate's
        // newest version, so it cannot see an older revision, and its output was
        // matched by substring — `1.16.0` "found" inside `1.16.0-1`.
        version::is_published(&SparseIndex, name, version).unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Core logic — takes any ReleaseChecker
// ---------------------------------------------------------------------------

/// Build a [`VerifyReport`] for the given version using the provided checker.
///
/// `crate_versions` lets callers override the expected version per crate
/// (useful when a partial publish left some crates at an older version).
/// Pass `None` to use `version` uniformly across all crates.
pub fn build_report(
    version: &str,
    repo: &str,
    crate_versions: Option<&[(String, String)]>,
    verbose: bool,
    checker: &dyn ReleaseChecker,
) -> VerifyReport {
    let repo_url = format!("https://github.com/{repo}");

    let release_branch_exists = checker.release_branch_exists(&repo_url, version);
    let github_release_exists = checker.github_release_exists(repo, version);

    let bare_version = version.trim_start_matches('v');

    let crates_published = CRATE_PUBLISH_ORDER
        .iter()
        .map(|crate_entry| {
            let crate_name = crate_name_from_path(crate_entry);
            let pkg_name = unofficial_name(crate_name);

            let expected_version = crate_versions
                .and_then(|pairs| {
                    pairs
                        .iter()
                        .find(|(n, _)| n == &pkg_name)
                        .map(|(_, v)| v.as_str())
                })
                .unwrap_or(bare_version);

            let published = checker.crate_version_published(&pkg_name, expected_version);

            if verbose {
                println!(
                    "  {} {}@{}",
                    status_icon(published),
                    pkg_name,
                    expected_version
                );
            }

            CrateStatus {
                name: pkg_name,
                version: expected_version.to_owned(),
                published,
            }
        })
        .collect();

    VerifyReport {
        release_branch_exists,
        github_release_exists,
        crates_published,
    }
}

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Verify a release and print a summary. Returns `true` if the release is
/// complete (for use as a CI exit-code signal via `std::process::exit`).
pub fn run(
    version: &str,
    repo: &str,
    crate_versions: Option<&[(String, String)]>,
    verbose: bool,
) -> Result<bool> {
    let tag = resolve_tag(version)?;
    println!("Verifying release {tag} in {repo}...\n");

    let report = build_report(&tag, repo, crate_versions, verbose, &LiveChecker);
    report.print_summary();

    let complete = report.is_complete();
    println!(
        "\nRelease {tag}: {}",
        if complete { "✓ COMPLETE" } else { "✗ INCOMPLETE" }
    );

    Ok(complete)
}

/// Turn the tag a caller asked about into the tag we actually released.
///
/// A bare upstream tag (`v1.16.0`) is resolved to the newest revision published
/// for it (`v1.16.0-2`), so CI can keep asking "is zed's latest release done?"
/// without knowing our revision counter. A tag that already carries a revision
/// is verified as given.
fn resolve_tag(tag: &str) -> Result<String> {
    if let Some(release) = version::ReleaseVersion::parse(tag) {
        return Ok(format!("v{release}"));
    }

    let upstream = version::strip_v(tag);
    let published = SparseIndex.published_versions("gpui-unofficial")?;
    Ok(resolved_tag_for(upstream, &published))
}

/// The tag half of [`resolve_tag`], without the network.
fn resolved_tag_for(upstream: &str, published: &[String]) -> String {
    let upstream = version::strip_v(upstream);

    // Releases from before the revision scheme are bare versions with no
    // revision to find. They are complete as they stand — resolving them to
    // `-0` would report every one of them as missing and re-release it at a
    // version that sorts *below* what is already published.
    if version::highest_revision(published, upstream).is_none()
        && published.iter().any(|v| v == upstream)
    {
        return format!("v{upstream}");
    }

    // Otherwise `Reuse` names the newest published revision, or -0 when nothing
    // is published for this upstream version yet — in which case every check
    // below fails, which is the answer the caller wants.
    format!(
        "v{}",
        version::resolve(upstream, published, RevisionMode::Reuse)
    )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    // --- Fake checker -------------------------------------------------------

    struct FakeChecker {
        branch_exists: bool,
        gh_release_exists: bool,
        /// Per-crate published state. Defaults to `true` for any crate not listed.
        crate_state: HashMap<String, bool>,
    }

    impl FakeChecker {
        fn all_published() -> Self {
            Self {
                branch_exists: true,
                gh_release_exists: true,
                crate_state: HashMap::new(),
            }
        }

        fn with_unpublished(mut self, name: &str) -> Self {
            self.crate_state.insert(name.to_string(), false);
            self
        }

        fn with_no_branch(mut self) -> Self {
            self.branch_exists = false;
            self
        }

        fn with_no_gh_release(mut self) -> Self {
            self.gh_release_exists = false;
            self
        }
    }

    impl ReleaseChecker for FakeChecker {
        fn release_branch_exists(&self, _repo_url: &str, _version: &str) -> bool {
            self.branch_exists
        }

        fn github_release_exists(&self, _repo: &str, _tag: &str) -> bool {
            self.gh_release_exists
        }

        fn crate_version_published(&self, name: &str, _version: &str) -> bool {
            *self.crate_state.get(name).unwrap_or(&true)
        }
    }

    // --- Helpers ------------------------------------------------------------

    fn run_report(checker: &dyn ReleaseChecker) -> VerifyReport {
        build_report("v1.8.2", "iamnbutler/gpui-unofficial", None, false, checker)
    }

    // --- Tests --------------------------------------------------------------

    #[test]
    fn complete_release_is_reported_complete() {
        let checker = FakeChecker::all_published();
        let report = run_report(&checker);
        assert!(report.is_complete());
        assert!(report.missing_crates().is_empty());
    }

    #[test]
    fn missing_branch_is_incomplete() {
        let checker = FakeChecker::all_published().with_no_branch();
        let report = run_report(&checker);
        assert!(!report.is_complete());
        assert!(!report.release_branch_exists);
        assert!(report.github_release_exists);
    }

    #[test]
    fn missing_github_release_is_incomplete() {
        let checker = FakeChecker::all_published().with_no_gh_release();
        let report = run_report(&checker);
        assert!(!report.is_complete());
        assert!(report.release_branch_exists);
        assert!(!report.github_release_exists);
    }

    #[test]
    fn unpublished_crate_is_reported_missing() {
        let checker = FakeChecker::all_published()
            .with_unpublished("gpui-platform-gpui-unofficial");
        let report = run_report(&checker);
        assert!(!report.is_complete());

        let missing = report.missing_crates();
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].name, "gpui-platform-gpui-unofficial");
    }

    #[test]
    fn multiple_unpublished_crates_all_reported() {
        let checker = FakeChecker::all_published()
            .with_unpublished("gpui-platform-gpui-unofficial")
            .with_unpublished("gpui-unofficial");
        let report = run_report(&checker);
        assert!(!report.is_complete());

        let missing_names: Vec<&str> = report.missing_crates().iter().map(|c| c.name.as_str()).collect();
        assert!(missing_names.contains(&"gpui-platform-gpui-unofficial"));
        assert!(missing_names.contains(&"gpui-unofficial"));
    }

    #[test]
    fn version_stripped_of_v_prefix_for_crates() {
        let checker = FakeChecker::all_published();
        let report = build_report("v1.8.2", "iamnbutler/gpui-unofficial", None, false, &checker);
        for c in &report.crates_published {
            assert_eq!(c.version, "1.8.2", "crate {} has wrong version string", c.name);
        }
    }

    #[test]
    fn bare_version_also_works() {
        let checker = FakeChecker::all_published();
        let report = build_report("1.8.2", "iamnbutler/gpui-unofficial", None, false, &checker);
        for c in &report.crates_published {
            assert_eq!(c.version, "1.8.2");
        }
    }

    #[test]
    fn per_crate_version_override_is_respected() {
        let checker = FakeChecker::all_published();
        let overrides = vec![
            ("gpui-unofficial".to_string(), "1.7.2".to_string()),
        ];
        let report = build_report("v1.8.2", "iamnbutler/gpui-unofficial", Some(&overrides), false, &checker);

        let gpui = report
            .crates_published
            .iter()
            .find(|c| c.name == "gpui-unofficial")
            .expect("gpui-unofficial should be in report");
        assert_eq!(gpui.version, "1.7.2");

        let other = report
            .crates_published
            .iter()
            .find(|c| c.name != "gpui-unofficial")
            .expect("at least one other crate");
        assert_eq!(other.version, "1.8.2");
    }

    // --- tag resolution -----------------------------------------------------

    fn published(versions: &[&str]) -> Vec<String> {
        versions.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bare_tag_resolves_to_the_newest_published_revision() {
        let published = published(&["1.16.0-0", "1.16.0-1"]);
        assert_eq!(resolved_tag_for("1.16.0", &published), "v1.16.0-1");
    }

    #[test]
    fn tag_with_a_revision_is_taken_as_given() {
        // Checked before any index lookup, so this needs no fixture.
        assert_eq!(
            version::ReleaseVersion::parse("v1.16.0-1").map(|r| r.to_string()),
            Some("1.16.0-1".to_string())
        );
    }

    #[test]
    fn unreleased_tag_resolves_to_revision_zero() {
        let published = published(&["1.16.0-0"]);
        // Nothing published for 1.17.0: the checks that follow all fail, which
        // is how CI learns there is a release to make.
        assert_eq!(resolved_tag_for("1.17.0", &published), "v1.17.0-0");
    }

    #[test]
    fn pre_scheme_releases_still_verify_as_themselves() {
        // 1.15.0 shipped before revisions existed. Resolving it to `1.15.0-0`
        // would report it as missing and trigger a re-release below itself.
        let published = published(&["1.14.2", "1.15.0"]);
        assert_eq!(resolved_tag_for("1.15.0", &published), "v1.15.0");
        assert_eq!(resolved_tag_for("v1.15.0", &published), "v1.15.0");
    }

    #[test]
    fn a_revision_supersedes_a_bare_version_of_the_same_release() {
        // If we ever do publish a revision on top of a pre-scheme version, that
        // revision is the release to verify.
        let published = published(&["1.15.0", "1.15.0-1"]);
        assert_eq!(resolved_tag_for("1.15.0", &published), "v1.15.0-1");
    }

    #[test]
    fn all_three_failures_reported_together() {
        let checker = FakeChecker {
            branch_exists: false,
            gh_release_exists: false,
            crate_state: {
                let mut m = HashMap::new();
                m.insert("gpui-unofficial".to_string(), false);
                m
            },
        };
        let report = run_report(&checker);
        assert!(!report.release_branch_exists);
        assert!(!report.github_release_exists);
        assert!(!report.crates_published.iter().all(|c| c.published));
        assert!(!report.is_complete());
    }
}
