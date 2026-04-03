use anyhow::{bail, Result};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use toml_edit::{DocumentMut, Item, Value};

use crate::transform::{CRATE_PUBLISH_ORDER, crate_name_from_path, unofficial_name};

/// crates.io allows a burst of 5 new crates, then 1 per 10 minutes.
/// For existing crates (version updates), the limit is more generous.
const NEW_CRATE_BURST: usize = 5;
/// Delay after the burst for new crates (10 min + buffer)
const NEW_CRATE_DELAY: Duration = Duration::from_secs(630);
/// Delay between publishes for propagation (crates.io sparse index takes 30-90s)
const PROPAGATION_DELAY: Duration = Duration::from_secs(90);
/// Max retries on rate limit or propagation failures
const MAX_RETRIES: usize = 3;
/// Initial backoff on rate limit (5 minutes)
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(300);
/// Backoff when a dependency hasn't propagated yet (60 seconds)
const PROPAGATION_BACKOFF: Duration = Duration::from_secs(60);

/// Before publishing, patch the crate's Cargo.toml to remove git source fields that
/// crates.io rejects. Operates in-place; cargo --allow-dirty picks up the changes.
///
/// Two cases:
/// - dep has `git` + `version`: strip git/rev/branch/tag, keep version
/// - dep has `git` only (no `version`): remove the dep entirely; if it was optional,
///   also purge references from [features]
fn patch_git_deps_for_publish(crate_dir: &Path) -> Result<()> {
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path)?;
    let mut doc: DocumentMut = content.parse()?;

    let mut removed_optional: Vec<String> = Vec::new();

    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
        patch_dep_section_git(&mut doc, section, &mut removed_optional);
    }

    // Handle target-specific dependency sections
    if let Some(target) = doc.get("target").cloned() {
        if let Some(target_table) = target.as_table_like() {
            let target_names: Vec<_> = target_table.iter().map(|(k, _)| k.to_string()).collect();
            for target_name in target_names {
                for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
                    // We need to patch in place via a temp doc trick
                    let maybe_deps = doc
                        .get("target")
                        .and_then(|t| t.get(&target_name))
                        .and_then(|t| t.get(section))
                        .cloned();
                    if let Some(deps_item) = maybe_deps {
                        let mut temp = DocumentMut::new();
                        temp.insert(section, deps_item);
                        patch_dep_section_git(&mut temp, section, &mut removed_optional);
                        if let Some(patched) = temp.get(section).cloned() {
                            if let Some(target_section) = doc
                                .get_mut("target")
                                .and_then(|t| t.get_mut(&target_name))
                                .and_then(|t| t.as_table_like_mut())
                            {
                                target_section.insert(section, patched);
                            }
                        }
                    }
                }
            }
        }
    }

    // Remove removed optional deps from [features]
    for dep_name in &removed_optional {
        remove_dep_from_features(&mut doc, dep_name);
    }

    fs::write(&cargo_toml_path, doc.to_string())?;
    Ok(())
}

fn patch_dep_section_git(doc: &mut DocumentMut, section: &str, removed_optional: &mut Vec<String>) {
    let Some(deps) = doc.get_mut(section) else { return };
    let Some(table) = deps.as_table_like_mut() else { return };

    let dep_names: Vec<_> = table.iter().map(|(k, _)| k.to_string()).collect();
    for dep_name in dep_names {
        let (has_git, has_version, is_optional) = if let Some(dep) = table.get(&dep_name) {
            let t = dep.as_table_like();
            (
                t.is_some_and(|t| t.get("git").is_some()),
                t.is_some_and(|t| t.get("version").is_some()),
                t.is_some_and(|t| t.get("optional").and_then(|v| v.as_bool()) == Some(true)),
            )
        } else {
            continue;
        };

        if !has_git {
            continue;
        }

        if has_version {
            // Strip git source fields, keep version — crates.io requires no git fields
            if let Some(dep) = table.get_mut(&dep_name) {
                if let Some(t) = dep.as_table_like_mut() {
                    t.remove("git");
                    t.remove("rev");
                    t.remove("branch");
                    t.remove("tag");
                }
            }
        } else {
            // Git-only dep (no version) — remove entirely; crates.io has no equivalent
            table.remove(&dep_name);
            if is_optional {
                removed_optional.push(dep_name);
            }
        }
    }
}

fn remove_dep_from_features(doc: &mut DocumentMut, dep_name: &str) {
    let Some(features) = doc.get_mut("features") else { return };
    let Some(table) = features.as_table_like_mut() else { return };

    let feature_names: Vec<_> = table.iter().map(|(k, _)| k.to_string()).collect();
    for feat_name in feature_names {
        if let Some(feat_val) = table.get_mut(&feat_name) {
            if let Some(arr) = feat_val.as_array_mut() {
                let to_keep: Vec<_> = arr
                    .iter()
                    .filter(|v| {
                        let s = v.as_str().unwrap_or("");
                        s != dep_name && !s.starts_with(&format!("{dep_name}/"))
                    })
                    .cloned()
                    .collect();
                arr.clear();
                for v in to_keep {
                    arr.push(v);
                }
            } else if let Some(Item::Value(Value::Array(arr))) = table.get_mut(&feat_name) {
                let to_keep: Vec<_> = arr
                    .iter()
                    .filter(|v| {
                        let s = v.as_str().unwrap_or("");
                        s != dep_name && !s.starts_with(&format!("{dep_name}/"))
                    })
                    .cloned()
                    .collect();
                arr.clear();
                for v in to_keep {
                    arr.push(v);
                }
            }
        }
    }
}

fn crate_exists_on_registry(name: &str) -> bool {
    Command::new("cargo")
        .args(["search", name, "--limit", "1"])
        .output()
        .ok()
        .is_some_and(|o| String::from_utf8_lossy(&o.stdout).contains(name))
}

pub fn run(crates_dir: &str, dry_run: bool) -> Result<()> {
    let crates_path = Path::new(crates_dir);

    if !crates_path.exists() {
        bail!("Crates directory not found: {crates_dir}");
    }

    println!(
        "Publishing {} crates {}",
        CRATE_PUBLISH_ORDER.len(),
        if dry_run { "(dry run)" } else { "" }
    );

    let mut new_crate_count = 0;

    for (i, crate_entry) in CRATE_PUBLISH_ORDER.iter().enumerate() {
        let crate_name = crate_name_from_path(crate_entry);
        let pkg_name = unofficial_name(crate_name);
        let crate_path = crates_path.join(&pkg_name);

        if !crate_path.exists() {
            println!("Skipping {pkg_name} (not found)");
            continue;
        }

        // Check rate limiting for new crates
        if !dry_run {
            let is_new = !crate_exists_on_registry(&pkg_name);
            if is_new {
                new_crate_count += 1;
                if new_crate_count > NEW_CRATE_BURST {
                    println!(
                        "Rate limit: new crate #{new_crate_count} (past burst of {NEW_CRATE_BURST}), waiting {NEW_CRATE_DELAY:?}..."
                    );
                    thread::sleep(NEW_CRATE_DELAY);
                }
            }
        }

        println!(
            "[{}/{}] Publishing {pkg_name}...",
            i + 1,
            CRATE_PUBLISH_ORDER.len()
        );

        // Patch out any git-source deps that crates.io would reject
        if !dry_run {
            patch_git_deps_for_publish(&crate_path)?;
        }

        let mut published = false;
        for attempt in 0..=MAX_RETRIES {
            let mut cmd = Command::new("cargo");
            cmd.args(["publish", "--allow-dirty"]);

            if dry_run {
                cmd.arg("--dry-run");
            }

            cmd.current_dir(&crate_path);

            let output = cmd.output()?;

            if output.status.success() {
                published = true;
                break;
            }

            let stderr = String::from_utf8_lossy(&output.stderr);

            // Version already published — skip
            if stderr.contains("already exists") || stderr.contains("already uploaded") {
                println!("  {pkg_name} already exists on crates.io, skipping.");
                break;
            }

            // Rate limited — backoff and retry
            if stderr.contains("rate limit")
                || stderr.contains("429")
                || stderr.contains("try again")
            {
                if attempt < MAX_RETRIES {
                    let wait = RATE_LIMIT_BACKOFF * (attempt as u32 + 1);
                    println!(
                        "  Rate limited, retrying in {wait:?} (attempt {}/{MAX_RETRIES})...",
                        attempt + 1
                    );
                    thread::sleep(wait);
                    continue;
                }
                bail!("Failed to publish {pkg_name} after {MAX_RETRIES} retries (rate limited)");
            }

            // Dependency not yet propagated — wait and retry
            if stderr.contains("not found in registry")
                || stderr.contains("no matching package")
                || stderr.contains("failed to select a version")
                || stderr.contains("package not found")
            {
                if attempt < MAX_RETRIES {
                    println!(
                        "  Dependency not yet propagated, retrying in {PROPAGATION_BACKOFF:?} (attempt {}/{MAX_RETRIES})...",
                        attempt + 1
                    );
                    thread::sleep(PROPAGATION_BACKOFF);
                    continue;
                }
                eprintln!("{stderr}");
                bail!("Failed to publish {pkg_name} after {MAX_RETRIES} retries (dependency propagation)");
            }

            // Some other failure
            eprintln!("{stderr}");
            bail!("Failed to publish {pkg_name}");
        }

        // Wait for crates.io propagation only if we actually published
        if published && !dry_run && i < CRATE_PUBLISH_ORDER.len() - 1 {
            println!("Waiting {PROPAGATION_DELAY:?} for crates.io propagation...");
            thread::sleep(PROPAGATION_DELAY);
        }
    }

    println!("\nPublish complete!");

    Ok(())
}
