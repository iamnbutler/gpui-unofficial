use anyhow::{bail, Result};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;
use toml_edit::DocumentMut;

use crate::transform::{lookup_crates_io_version, remove_dep_from_features, CRATE_PUBLISH_ORDER, crate_name_from_path, unofficial_name};

/// crates.io allows a burst of 5 new crates, then 1 per 10 minutes.
/// For existing crates (version updates), the limit is more generous.
const NEW_CRATE_BURST: usize = 5;
/// Delay after the burst for new crates (10 min + buffer)
const NEW_CRATE_DELAY: Duration = Duration::from_secs(630);
/// Delay between publishes for propagation (crates.io sparse index takes 30–90s)
const PROPAGATION_DELAY: Duration = Duration::from_secs(90);
/// Backoff between retries when a dependency hasn't propagated yet
const PROPAGATION_RETRY_WAIT: Duration = Duration::from_secs(90);
/// Max retries on rate limit or propagation failures
const MAX_RETRIES: usize = 5;
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

    // Strip dev-dependencies on internal crates — they create circular publish
    // ordering issues (e.g. gpui-macros dev-depends on gpui, but gpui depends on
    // gpui-macros). Dev-deps aren't needed by consumers of the published crate.
    strip_internal_dev_deps(&mut doc, "dev-dependencies");

    // Also strip target-specific dev-dependencies on internal crates
    let target_names: Vec<String> = doc
        .get("target")
        .and_then(|t| t.as_table_like())
        .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
        .unwrap_or_default();

    for target_name in &target_names {
        let has_dev_deps = doc
            .get("target")
            .and_then(|t| t.get(target_name))
            .and_then(|s| s.get("dev-dependencies"))
            .is_some();

        if has_dev_deps {
            let deps_item = doc["target"][target_name]["dev-dependencies"].clone();
            let mut temp_doc = DocumentMut::new();
            temp_doc.insert("dev-dependencies", deps_item);
            strip_internal_dev_deps(&mut temp_doc, "dev-dependencies");
            if let Some(new_deps) = temp_doc.get("dev-dependencies").cloned() {
                if let Some(target_section) = doc
                    .get_mut("target")
                    .and_then(|t| t.get_mut(target_name))
                    .and_then(|s| s.as_table_like_mut())
                {
                    target_section.insert("dev-dependencies", new_deps);
                }
            }
        }
    }

    fs::write(&cargo_toml_path, doc.to_string())?;
    Ok(())
}

/// Remove any dev-dependencies that reference internal (workspace) crates.
fn strip_internal_dev_deps(doc: &mut DocumentMut, section: &str) {
    let to_remove: Vec<String> = doc
        .get(section)
        .and_then(|d| d.as_table_like())
        .map(|table| {
            table
                .iter()
                .filter_map(|(name, dep)| {
                    // Check the `package` field (alias) or the dep key itself
                    let pkg = dep
                        .as_table_like()
                        .and_then(|t| t.get("package"))
                        .and_then(|v| v.as_str())
                        .unwrap_or(name);
                    // Match against unofficial naming convention
                    let is_ours = pkg.ends_with("-gpui-unofficial") || pkg == "gpui-unofficial";
                    is_ours.then(|| name.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    if to_remove.is_empty() {
        return;
    }

    if let Some(deps) = doc.get_mut(section).and_then(|d| d.as_table_like_mut()) {
        for name in &to_remove {
            println!("  Stripping internal dev-dep: {name}");
            deps.remove(name);
        }
    }
}

/// Prepend an unofficial release notice to a crate's README before publishing.
fn patch_readme_for_publish(crate_dir: &Path, original_name: &str) {
    let readme_path = crate_dir.join("README.md");
    let original_name_kebab = original_name.replace('_', "-");
    let notice = format!(
        "> **Note:** This is an unofficial release of Zed's \
         [{original_name}](https://github.com/zed-industries/zed/tree/main/crates/{original_name}) crate, \
         published to crates.io by [gpui-unofficial](https://github.com/iamnbutler/gpui-unofficial). \
         It is not maintained by the Zed team. For issues with the crate itself, see the \
         [Zed repository](https://github.com/zed-industries/zed).\n\n"
    );

    let content = fs::read_to_string(&readme_path).unwrap_or_default();
    if content.contains("unofficial release") {
        return; // Already patched
    }

    // Prepend notice, replacing the first heading if it exists
    let patched = if let Some(rest) = content.strip_prefix("# ") {
        // Replace original heading with unofficial name, then add notice
        let heading_end = rest.find('\n').unwrap_or(rest.len());
        format!(
            "# {original_name_kebab}-gpui-unofficial\n\n{notice}{}\n",
            &rest[heading_end..].trim_start_matches('\n')
        )
    } else {
        format!("{notice}{content}")
    };

    let _ = fs::write(&readme_path, patched);
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
                        } else {
                            // Git-only dep (no version). Try to find a crates.io equivalent.
                            let pkg_name = dep_table
                                .get("package")
                                .and_then(|v| v.as_str())
                                .unwrap_or(dep_name);
                            if let Some(ver) = lookup_crates_io_version(pkg_name)
                                .or_else(|| known_fork_version(pkg_name))
                            {
                                println!("  Replacing git-only dep '{dep_name}' with crates.io {pkg_name}@{ver}");
                                to_replace.push((dep_name.to_string(), ver));
                            } else if section == "dependencies" && !is_optional {
                                // Non-optional core dep with no crates.io equivalent — remove
                                // (will likely fail to compile)
                                eprintln!("  WARNING: git-only dep '{dep_name}' has no crates.io equivalent; removing");
                                to_remove.push(dep_name.to_string());
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
                if let Some(dep) = table.get_mut(dep_name) {
                    if let Some(dep_table) = dep.as_table_like_mut() {
                        // Strip git fields and add version, preserving optional/features/etc.
                        dep_table.remove("git");
                        dep_table.remove("rev");
                        dep_table.remove("branch");
                        dep_table.remove("tag");
                        dep_table.insert("version", toml_edit::value(ver.as_str()));
                    }
                }
            }
        }
    }
}

/// Fallback version mapping for known Zed git forks when cargo search fails.
/// These are well-known forks that track the official crates.io releases.
fn known_fork_version(package: &str) -> Option<String> {
    match package {
        "wgpu" => Some("29".to_string()),
        "zed-font-kit" => Some("0.14".to_string()),
        "zed-scap" => Some("0.0.8".to_string()),
        "proptest" => Some("1".to_string()),
        _ => None,
    }
}

fn crate_exists_on_registry(name: &str) -> bool {
    // Retry up to 3 times with short backoff to handle crates.io rate limits
    for attempt in 0..3 {
        if attempt > 0 {
            thread::sleep(Duration::from_secs(5 * attempt as u64));
        }
        match Command::new("cargo")
            .args(["search", name, "--limit", "1"])
            .output()
        {
            Ok(o) if !o.stdout.is_empty() => {
                return String::from_utf8_lossy(&o.stdout).contains(name);
            }
            Ok(_) => return false, // Empty result = not found
            Err(_) => continue,
        }
    }
    false
}

/// Check if a specific crate version exists on crates.io
fn crate_version_exists(name: &str, version: &str) -> bool {
    // Use cargo info which checks the exact version
    Command::new("cargo")
        .args(["search", &format!("{name}"), "--limit", "1"])
        .output()
        .ok()
        .is_some_and(|o| {
            let stdout = String::from_utf8_lossy(&o.stdout);
            // cargo search returns: name = "version"
            stdout.contains(name) && stdout.contains(version)
        })
}

/// Run only the Cargo.toml patching step (strip git deps) without publishing.
/// Useful for verifying that the patched files have no git deps before a real publish.
pub fn patch_only(crates_dir: &str) -> Result<()> {
    let crates_path = Path::new(crates_dir);
    if !crates_path.exists() {
        bail!("Crates directory not found: {crates_dir}");
    }

    println!("Patching {} crates (no publish)", CRATE_PUBLISH_ORDER.len());

    for (i, crate_entry) in CRATE_PUBLISH_ORDER.iter().enumerate() {
        let crate_name = crate_name_from_path(crate_entry);
        let pkg_name = unofficial_name(crate_name);
        let crate_path = crates_path.join(&pkg_name);

        if !crate_path.exists() {
            println!("  Skipping {pkg_name} (not found)");
            continue;
        }

        println!("[{}/{}] Patching {pkg_name}...", i + 1, CRATE_PUBLISH_ORDER.len());
        patch_git_deps_for_publish(&crate_path)?;
        patch_readme_for_publish(&crate_path, crate_name);
    }

    println!("\nPatch complete! Inspect crates/ to verify.");
    Ok(())
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

        // Read version from the crate's Cargo.toml
        let cargo_toml_content = fs::read_to_string(crate_path.join("Cargo.toml"))?;
        let crate_version = cargo_toml_content
            .parse::<DocumentMut>()
            .ok()
            .and_then(|doc| {
                doc.get("package")
                    .and_then(|p| p.get("version"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();

        // Skip early if this exact version is already published — avoids
        // unnecessary cargo search calls and git-dep patching
        if !dry_run && crate_version_exists(&pkg_name, &crate_version) {
            println!(
                "[{}/{}] {pkg_name}@{crate_version} already on crates.io, skipping.",
                i + 1,
                CRATE_PUBLISH_ORDER.len()
            );
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

        // Patch README to note this is an unofficial release
        patch_readme_for_publish(&crate_path, crate_name);

        println!(
            "[{}/{}] Publishing {pkg_name}...",
            i + 1,
            CRATE_PUBLISH_ORDER.len()
        );

        // Use --no-verify for crates that had git deps replaced with crates.io
        // versions — the Zed fork APIs may differ from the official release,
        // so verification (compilation) may fail even though the metadata is correct.
        let patched_toml = fs::read_to_string(crate_path.join("Cargo.toml"))?;
        let needs_no_verify = patched_toml.contains("# git dep replaced");
        // Also check if any git deps were replaced by the known_fork_version fallback
        let has_forked_deps = ["gpui_wgpu", "gpui_macos", "gpui_linux", "gpui_windows", "gpui_web", "gpui_platform"]
            .iter()
            .any(|name| crate_name == *name);

        let mut published = false;
        for attempt in 0..=MAX_RETRIES {
            let mut cmd = Command::new("cargo");
            cmd.args(["publish", "--allow-dirty"]);

            // Skip verification for platform crates that depend on forked deps
            if has_forked_deps || needs_no_verify {
                cmd.arg("--no-verify");
            }

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
                    // Extract the missing package name for diagnostics
                    let missing = stderr
                        .lines()
                        .find(|l| l.contains("no matching package") || l.contains("not found"))
                        .unwrap_or("(unknown)");
                    println!(
                        "  Dependency not yet propagated: {missing}");
                    println!(
                        "  Retrying in {PROPAGATION_RETRY_WAIT:?} (attempt {}/{MAX_RETRIES})...",
                        attempt + 1
                    );
                    thread::sleep(PROPAGATION_RETRY_WAIT);
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
