use anyhow::{bail, Result};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use toml_edit::{DocumentMut, Item};

use crate::transform::{lookup_crates_io_version, remove_dep_from_features, CRATE_PUBLISH_ORDER, crate_name_from_path, unofficial_name};

/// crates.io allows a burst of 5 new crates, then 1 per 10 minutes.
/// For existing crates (version updates), the limit is more generous.
const NEW_CRATE_BURST: usize = 5;
/// Delay after the burst for new crates (10 min + buffer)
const NEW_CRATE_DELAY: Duration = Duration::from_secs(630);
/// Delay between publishes for propagation (crates.io sparse index takes 30–90s)
const PROPAGATION_DELAY: Duration = Duration::from_secs(90);
/// Backoff between retries when a dependency hasn't propagated yet
const PROPAGATION_RETRY_WAIT: Duration = Duration::from_secs(60);
/// Max retries on rate limit or propagation failures
const MAX_RETRIES: usize = 3;
/// Initial backoff on rate limit (5 minutes)
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(300);

/// Strip or remove git dependencies from a generated Cargo.toml before publishing.
///
/// crates.io rejects any dependency that contains a `git` field, even when a `version`
/// is also present. This patches the already-generated files in place:
/// - git+version deps: strips `git`/`rev`/`branch`/`tag`, keeps `version`
/// - git-only deps (no version): removes the dep entirely and cleans up `[features]`
fn patch_git_deps_for_publish(crate_dir: &Path) -> Result<()> {
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path)?;
    let mut doc: DocumentMut = content.parse()?;
    let mut removed_optionals: Vec<String> = Vec::new();

    // Standard top-level sections
    for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
        patch_dep_section_git(&mut doc, section, &mut removed_optionals);
    }

    // Target-specific sections
    let target_names: Vec<String> = doc
        .get("target")
        .and_then(|t| t.as_table_like())
        .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default();

    for target_name in &target_names {
        for section in &["dependencies", "dev-dependencies", "build-dependencies"] {
            let maybe_deps = doc
                .get("target")
                .and_then(|t| t.get(target_name))
                .and_then(|s| s.get(section))
                .cloned();

            if let Some(deps_item) = maybe_deps {
                let mut temp_doc = DocumentMut::new();
                temp_doc.insert(section, deps_item);
                patch_dep_section_git(&mut temp_doc, section, &mut removed_optionals);
                if let Some(new_deps) = temp_doc.get(section).cloned() {
                    if let Some(target_section) = doc
                        .get_mut("target")
                        .and_then(|t| t.get_mut(target_name))
                        .and_then(|s| s.as_table_like_mut())
                    {
                        target_section.insert(section, new_deps);
                    }
                }
            }
        }
    }

    // Remove removed optional deps from [features]
    for dep_name in &removed_optionals {
        remove_dep_from_features(&mut doc, dep_name);
    }

    fs::write(&cargo_toml_path, doc.to_string())?;
    Ok(())
}

/// Patch a single dependency section: strip git fields from git+version deps,
/// and for git-only deps: replace non-optional [dependencies] with the crates.io
/// version (via `cargo search`), or remove optional/dev deps entirely.
fn patch_dep_section_git(doc: &mut DocumentMut, section: &str, removed_optionals: &mut Vec<String>) {
    // Phase 1: identify what to strip, remove, or replace with a crates.io version
    let mut to_strip: Vec<String> = Vec::new();
    let mut to_remove: Vec<String> = Vec::new();
    let mut to_replace: Vec<(String, String)> = Vec::new(); // (dep_name, crates_io_version)

    if let Some(deps) = doc.get(section) {
        if let Some(table) = deps.as_table_like() {
            for (dep_name, dep) in table.iter() {
                if let Some(dep_table) = dep.as_table_like() {
                    if dep_table.get("git").is_some() {
                        let has_version = dep_table.get("version").is_some();
                        let is_optional = dep_table
                            .get("optional")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        if has_version {
                            to_strip.push(dep_name.to_string());
                        } else if !is_optional && section == "dependencies" {
                            // Non-optional core dep with no version — look up on crates.io.
                            // The zed-industries fork (e.g. wgpu branch="v29") tracks the
                            // official crates.io release of the same major version.
                            let pkg_name = dep_table
                                .get("package")
                                .and_then(|v| v.as_str())
                                .unwrap_or(dep_name);
                            if let Some(ver) = lookup_crates_io_version(pkg_name) {
                                println!("  Replacing git-only dep '{dep_name}' with crates.io {pkg_name}@{ver}");
                                to_replace.push((dep_name.to_string(), ver));
                            } else {
                                // No crates.io equivalent found — remove (will likely fail to compile)
                                eprintln!("  WARNING: git-only dep '{dep_name}' has no crates.io equivalent; removing");
                                to_remove.push(dep_name.to_string());
                            }
                        } else {
                            if is_optional {
                                removed_optionals.push(dep_name.to_string());
                            }
                            to_remove.push(dep_name.to_string());
                        }
                    }
                }
            }
        }
    }

    // Phase 2: apply changes
    if let Some(deps) = doc.get_mut(section) {
        if let Some(table) = deps.as_table_like_mut() {
            for dep_name in &to_strip {
                if let Some(dep) = table.get_mut(dep_name) {
                    if let Some(dep_table) = dep.as_table_like_mut() {
                        dep_table.remove("git");
                        dep_table.remove("rev");
                        dep_table.remove("branch");
                        dep_table.remove("tag");
                    }
                }
            }
            for dep_name in &to_remove {
                table.remove(dep_name);
            }
            for (dep_name, ver) in &to_replace {
                let mut new_dep = toml_edit::InlineTable::new();
                new_dep.insert("version", ver.as_str().into());
                table.insert(dep_name, Item::Value(toml_edit::Value::InlineTable(new_dep)));
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

        // Patch any git dependencies in the already-generated Cargo.toml before publishing.
        // The transform may have baked in git fields that crates.io rejects.
        patch_git_deps_for_publish(&crate_path)?;

        println!(
            "[{}/{}] Publishing {pkg_name}...",
            i + 1,
            CRATE_PUBLISH_ORDER.len()
        );

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

            // Dependency not yet propagated to the sparse index — wait and retry
            if stderr.contains("not found in registry")
                || stderr.contains("no matching package")
                || stderr.contains("not available in any registry")
            {
                if attempt < MAX_RETRIES {
                    println!(
                        "  Dependency not yet propagated, retrying in {PROPAGATION_RETRY_WAIT:?} (attempt {}/{MAX_RETRIES})...",
                        attempt + 1
                    );
                    thread::sleep(PROPAGATION_RETRY_WAIT);
                    continue;
                }
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
