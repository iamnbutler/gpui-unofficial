use anyhow::Result;
use std::process::Command;

use crate::transform::{crate_name_from_path, unofficial_name, CRATE_PUBLISH_ORDER};

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
        Command::new("cargo")
            .args(["search", name, "--limit", "1"])
            .output()
            .ok()
            .is_some_and(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout.contains(name) && stdout.contains(version)
            })
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
    println!("Verifying release {version} in {repo}...\n");

    let report = build_report(version, repo, crate_versions, verbose, &LiveChecker);
    report.print_summary();

    let complete = report.is_complete();
    println!(
        "\nRelease {version}: {}",
        if complete { "✓ COMPLETE" } else { "✗ INCOMPLETE" }
    );

    Ok(complete)
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
