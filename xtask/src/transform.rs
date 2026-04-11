use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use toml_edit::{DocumentMut, Item, Value};
use walkdir::WalkDir;

/// Crates to extract and publish, in topological order (dependencies first)
pub const CRATE_PUBLISH_ORDER: &[&str] = &[
    // Tier 1 - Leaf crates
    "gpui_util",
    "collections",
    "refineable/derive_refineable",
    "refineable",
    "tooling/perf",
    "util_macros",
    "util",
    "zlog",
    "ztracing_macro",
    "ztracing",
    // Tier 2 - Core infrastructure
    "scheduler",
    "sum_tree",
    "http_client",
    "http_client_tls",
    "reqwest_client",
    "media",
    // Tier 3 - Main crates
    "gpui_macros",
    "gpui",
    // Tier 4 - Platform backends
    "gpui_wgpu",
    "gpui_macos",
    "gpui_linux",
    "gpui_windows",
    "gpui_web",
    // Tier 5 - Facade
    "gpui_platform",
];

/// Map from original crate name to unofficial name
pub fn unofficial_name(name: &str) -> String {
    if name == "gpui" {
        return "gpui-unofficial".to_string();
    }
    let kebab = name.replace('_', "-");
    format!("{kebab}-gpui-unofficial")
}

pub fn run(zed_tag: &str, zed_path: Option<&str>, output_dir: &str, use_local_deps: bool) -> Result<()> {
    println!("Transforming gpui from zed tag: {zed_tag}");
    if use_local_deps {
        println!("Using path dependencies for local testing");
    }

    // Get or clone zed repo
    let zed_dir = match zed_path {
        Some(path) => {
            // Use local path as-is (assume already at correct version)
            let path = PathBuf::from(path);
            println!("Using local zed at: {}", path.display());
            path
        }
        None => clone_zed(zed_tag)?,
    };

    // Parse zed's root Cargo.toml to get workspace dependency versions
    let workspace_deps = parse_workspace_deps(&zed_dir)?;
    println!("Parsed {} workspace dependencies", workspace_deps.len());

    // Create output directory
    let output_path = PathBuf::from(output_dir);
    if output_path.exists() {
        fs::remove_dir_all(&output_path)?;
    }
    fs::create_dir_all(&output_path)?;

    // Transform each crate
    for crate_name in CRATE_PUBLISH_ORDER {
        println!("Transforming: {crate_name}");
        transform_crate(&zed_dir, &output_path, crate_name, &workspace_deps, zed_tag, use_local_deps)?;
    }

    // Write metadata file
    write_metadata(&output_path, zed_tag, &zed_dir)?;

    println!("\nTransform complete! Crates written to: {output_dir}");
    println!("Run 'cargo build --workspace' to verify.");

    Ok(())
}

fn clone_zed(tag: &str) -> Result<PathBuf> {
    let temp_dir = tempfile::tempdir()?;
    let path = temp_dir.keep();

    println!("Cloning zed at tag {tag}...");
    let status = Command::new("git")
        .args([
            "clone",
            "--depth=1",
            "--branch",
            tag,
            "https://github.com/zed-industries/zed.git",
            path.to_str().unwrap(),
        ])
        .status()?;

    if !status.success() {
        bail!("Failed to clone zed repository");
    }

    Ok(path)
}

fn parse_workspace_deps(zed_dir: &Path) -> Result<HashMap<String, toml_edit::Item>> {
    let cargo_toml_path = zed_dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path)
        .with_context(|| format!("Failed to read {}", cargo_toml_path.display()))?;
    let doc: DocumentMut = content.parse()?;

    let mut deps = HashMap::new();

    // Extract [workspace.dependencies]
    if let Some(workspace) = doc.get("workspace") {
        if let Some(workspace_deps) = workspace.get("dependencies") {
            if let Some(table) = workspace_deps.as_table_like() {
                for (name, value) in table.iter() {
                    deps.insert(name.to_string(), value.clone());
                }
            }
        }
    }

    Ok(deps)
}

fn transform_crate(
    zed_dir: &Path,
    output_dir: &Path,
    crate_path: &str,
    workspace_deps: &HashMap<String, Item>,
    zed_tag: &str,
    use_local_deps: bool,
) -> Result<()> {
    // Handle paths that start with "tooling/" specially
    let src_dir = if crate_path.starts_with("tooling/") {
        zed_dir.join(crate_path)
    } else {
        zed_dir.join("crates").join(crate_path)
    };
    if !src_dir.exists() {
        bail!("Crate not found: {}", src_dir.display());
    }

    // Extract just the crate name from path (e.g., "refineable/derive_refineable" -> "derive_refineable")
    let crate_name = crate_path.rsplit('/').next().unwrap_or(crate_path);
    let unofficial = unofficial_name(crate_name);
    let dest_dir = output_dir.join(&unofficial);

    // Copy crate directory
    copy_dir_recursive(&src_dir, &dest_dir)?;

    // Transform Cargo.toml
    transform_cargo_toml(&dest_dir, output_dir, crate_name, workspace_deps, zed_tag, use_local_deps)?;

    // Note: Source files don't need transformation because we use Cargo.toml package aliasing
    // e.g., `collections = { package = "collections-unofficial", ... }`
    // This lets code keep using `use collections::...`

    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;

    for entry in WalkDir::new(src) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(src)?;
        let target = dest.join(relative);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)?;
        }
    }

    Ok(())
}

fn transform_cargo_toml(
    crate_dir: &Path,
    output_dir: &Path,
    original_name: &str,
    workspace_deps: &HashMap<String, Item>,
    zed_tag: &str,
    use_local_deps: bool,
) -> Result<()> {
    let cargo_toml_path = crate_dir.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path)?;
    let mut doc: DocumentMut = content.parse()?;

    let unofficial = unofficial_name(original_name);
    let version = zed_tag_to_version(zed_tag);

    // Update [package] section
    if let Some(package) = doc.get_mut("package") {
        if let Some(table) = package.as_table_like_mut() {
            // Rename package
            table.insert("name", toml_edit::value(&unofficial));

            // Set version
            table.insert("version", toml_edit::value(&version));

            // Remove workspace inheritance for edition, use explicit
            if table.get("edition").is_some_and(|v| v.as_table_like().is_some()) {
                table.insert("edition", toml_edit::value("2024"));
            }

            // Set repository
            table.insert(
                "repository",
                toml_edit::value("https://github.com/iamnbutler/gpui-unofficial"),
            );

            // Remove publish = false if present
            table.remove("publish");

            // Ensure license is set
            if !table.contains_key("license") {
                table.insert("license", toml_edit::value("Apache-2.0"));
            }

            // Ensure description is set (required by crates.io)
            if !table.contains_key("description") {
                table.insert(
                    "description",
                    toml_edit::value(format!("Unofficial release of Zed's {original_name} crate")),
                );
            }
        }
    }

    // Transform dependencies, collecting any optional deps that get removed (git-only, no crates.io equiv)
    let mut removed_optionals: Vec<String> = Vec::new();
    transform_dependencies(&mut doc, "dependencies", workspace_deps, &version, output_dir, use_local_deps, &mut removed_optionals)?;
    transform_dependencies(&mut doc, "dev-dependencies", workspace_deps, &version, output_dir, use_local_deps, &mut removed_optionals)?;
    transform_dependencies(&mut doc, "build-dependencies", workspace_deps, &version, output_dir, use_local_deps, &mut removed_optionals)?;

    // Handle target-specific dependencies
    if let Some(target) = doc.get_mut("target") {
        if let Some(target_table) = target.as_table_like_mut() {
            let targets: Vec<_> = target_table.iter().map(|(k, _)| k.to_string()).collect();
            for target_name in targets {
                if let Some(target_section) = doc.get_mut("target")
                    .and_then(|t| t.get_mut(&target_name))
                {
                    if let Some(table) = target_section.as_table_like_mut() {
                        for dep_section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                            if table.contains_key(dep_section) {
                                let mut temp_doc = DocumentMut::new();
                                if let Some(deps) = table.get(dep_section).cloned() {
                                    temp_doc.insert(dep_section, deps);
                                    transform_dependencies(&mut temp_doc, dep_section, workspace_deps, &version, output_dir, use_local_deps, &mut removed_optionals)?;
                                    if let Some(new_deps) = temp_doc.get(dep_section).cloned() {
                                        table.insert(dep_section, new_deps);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Clean up [features] entries that referenced removed optional deps
    for dep_name in &removed_optionals {
        remove_dep_from_features(&mut doc, dep_name);
    }

    // Remove inspector feature from gpui_macros and gpui
    if original_name == "gpui_macros" || original_name == "gpui" {
        remove_inspector_feature(&mut doc);
    }

    // Add proptest dependency to crates that need it for tests
    if original_name == "gpui" || original_name == "sum_tree" {
        add_proptest_dependency(&mut doc);
    }

    // Remove workspace lints (not supported for standalone crates)
    doc.remove("lints");

    // Add empty [workspace] to make crate independent
    doc.insert("workspace", Item::Table(toml_edit::Table::new()));

    // Write back
    fs::write(&cargo_toml_path, doc.to_string())?;

    Ok(())
}

/// Extract the crate name from a publish order entry (handles nested paths)
pub fn crate_name_from_path(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Check if a dependency name matches any crate in our publish order
pub fn is_internal_crate(dep_name: &str) -> bool {
    CRATE_PUBLISH_ORDER
        .iter()
        .any(|path| crate_name_from_path(path) == dep_name)
}

fn transform_dependencies(
    doc: &mut DocumentMut,
    section: &str,
    workspace_deps: &HashMap<String, Item>,
    version: &str,
    _output_dir: &Path,
    use_local_deps: bool,
    removed_optionals: &mut Vec<String>,
) -> Result<()> {
    let Some(deps) = doc.get_mut(section) else {
        return Ok(());
    };

    let Some(deps_table) = deps.as_table_like_mut() else {
        return Ok(());
    };

    let dep_names: Vec<_> = deps_table.iter().map(|(k, _)| k.to_string()).collect();
    let mut deps_to_remove: Vec<String> = Vec::new();

    for dep_name in dep_names {
        let is_internal = is_internal_crate(&dep_name);

        if let Some(dep) = deps_table.get_mut(&dep_name) {
            // Check if it's a workspace dependency
            let is_workspace = dep
                .as_table_like()
                .is_some_and(|t| t.get("workspace").is_some_and(|v| v.as_bool() == Some(true)))
                || dep.as_str() == Some("workspace = true");

            if is_workspace || dep.get("workspace").is_some() {
                if is_internal {
                    // Internal crate - use package alias so code can keep using original name
                    let unofficial = unofficial_name(&dep_name);
                    let mut new_dep = toml_edit::InlineTable::new();
                    new_dep.insert("package", unofficial.as_str().into());

                    if use_local_deps {
                        // Use path dependency for local testing (relative to sibling crate)
                        let relative_path = format!("../{unofficial}");
                        new_dep.insert("path", relative_path.into());
                    } else {
                        // Use version for publishing
                        new_dep.insert("version", version.into());
                    }

                    // Preserve features if any
                    if let Some(table) = dep.as_table_like() {
                        if let Some(features) = table.get("features") {
                            if let Some(arr) = features.as_array() {
                                let mut feat_arr = toml_edit::Array::new();
                                for f in arr.iter() {
                                    // Skip inspector feature
                                    if f.as_str() != Some("inspector") {
                                        feat_arr.push(f.clone());
                                    }
                                }
                                if !feat_arr.is_empty() {
                                    new_dep.insert("features", toml_edit::Value::Array(feat_arr));
                                }
                            }
                        }
                        if let Some(optional) = table.get("optional") {
                            if let Some(b) = optional.as_bool() {
                                new_dep.insert("optional", b.into());
                            }
                        }
                    }

                    // Keep the original name as the key (for aliasing)
                    deps_table.insert(&dep_name, Item::Value(Value::InlineTable(new_dep)));
                } else {
                    // External crate - resolve from workspace
                    if let Some(workspace_dep) = workspace_deps.get(&dep_name) {
                        // Check optional before passing dep to resolve (borrow ends after call)
                        let is_optional = dep.as_table_like()
                            .and_then(|t| t.get("optional"))
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false);
                        match resolve_workspace_dep(workspace_dep, dep)? {
                            Some(resolved) => {
                                deps_table.insert(&dep_name, resolved);
                            }
                            None => {
                                // Git-only dep with no version field.
                                // For non-optional [dependencies], try to find the official crates.io
                                // version (e.g. the zed-industries/wgpu fork tracks wgpu 29.x on crates.io).
                                let resolved_via_lookup = if !is_optional && section == "dependencies" {
                                    let pkg = workspace_dep
                                        .as_table_like()
                                        .and_then(|t| t.get("package"))
                                        .and_then(|v| v.as_str())
                                        .unwrap_or(&dep_name)
                                        .to_string();
                                    lookup_crates_io_version(&pkg).map(|ver| {
                                        println!("  Resolved git-only dep '{dep_name}' to crates.io {pkg}@{ver}");
                                        let mut t = toml_edit::InlineTable::new();
                                        t.insert("version", ver.as_str().into());
                                        Item::Value(Value::InlineTable(t))
                                    })
                                } else {
                                    None
                                };
                                if let Some(resolved) = resolved_via_lookup {
                                    deps_table.insert(&dep_name, resolved);
                                } else {
                                    if is_optional {
                                        removed_optionals.push(dep_name.clone());
                                    }
                                    deps_to_remove.push(dep_name.clone());
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Remove git-only deps after the loop (borrow of individual deps has ended)
    let Some(deps) = doc.get_mut(section) else {
        return Ok(());
    };
    let Some(deps_table) = deps.as_table_like_mut() else {
        return Ok(());
    };
    for dep_name in deps_to_remove {
        deps_table.remove(&dep_name);
    }

    Ok(())
}

fn resolve_workspace_dep(workspace_def: &Item, usage: &Item) -> Result<Option<Item>> {
    // Get the base definition from workspace.
    // Git fields (git/rev/branch/tag) are intentionally NOT copied — crates.io rejects them.
    // For git+version deps the version alone is sufficient.
    // For git-only deps (no version), we return None so the caller removes the dep.
    let mut result = if let Some(version) = workspace_def.as_str() {
        // Simple version string
        let mut table = toml_edit::InlineTable::new();
        table.insert("version", version.into());
        Item::Value(Value::InlineTable(table))
    } else if let Some(table) = workspace_def.as_table_like() {
        // Table with version and/or git fields
        let mut new_table = toml_edit::InlineTable::new();

        // Copy version if present (git fields intentionally omitted)
        if let Some(version) = table.get("version").and_then(|v| v.as_str()) {
            new_table.insert("version", version.into());
        }

        // Copy package rename if present
        if let Some(pkg) = table.get("package").and_then(|v| v.as_str()) {
            new_table.insert("package", pkg.into());
        }

        // Copy default-features if present
        if let Some(default_features) = table.get("default-features") {
            if let Some(b) = default_features.as_bool() {
                new_table.insert("default-features", b.into());
            }
        }

        // Copy features from workspace definition
        if let Some(features) = table.get("features") {
            if let Some(arr) = features.as_array() {
                let mut feat_arr = toml_edit::Array::new();
                for f in arr.iter() {
                    feat_arr.push(f.clone());
                }
                new_table.insert("features", toml_edit::Value::Array(feat_arr));
            }
        }

        // If there's no version and no path, this is a git-only dep — not publishable to crates.io
        if !new_table.contains_key("version") && !new_table.contains_key("path") {
            return Ok(None);
        }

        Item::Value(Value::InlineTable(new_table))
    } else {
        workspace_def.clone()
    };

    // Merge features from usage
    if let Some(usage_table) = usage.as_table_like() {
        if let Some(result_table) = result.as_table_like_mut() {
            if let Some(features) = usage_table.get("features") {
                if let Some(arr) = features.as_array() {
                    let mut feat_arr = toml_edit::Array::new();
                    for f in arr.iter() {
                        feat_arr.push(f.clone());
                    }
                    result_table.insert("features", Item::Value(Value::Array(feat_arr)));
                }
            }
            if let Some(optional) = usage_table.get("optional") {
                if let Some(b) = optional.as_bool() {
                    result_table.insert("optional", Item::Value(Value::from(b)));
                }
            }
        }
    }

    Ok(Some(result))
}

/// Remove all references to a dep from the `[features]` section.
/// Handles both bare `"dep_name"` activations and `"dep_name/feature"` entries.
pub(crate) fn remove_dep_from_features(doc: &mut DocumentMut, dep_name: &str) {
    // Phase 1: collect which features need a new array
    let mut modifications: Vec<(String, toml_edit::Array)> = Vec::new();
    if let Some(features) = doc.get("features") {
        if let Some(table) = features.as_table_like() {
            for (feat_name, feat_val) in table.iter() {
                let arr = feat_val
                    .as_value()
                    .and_then(|v| v.as_array())
                    .or_else(|| feat_val.as_array());
                if let Some(arr) = arr {
                    let mut new_arr = toml_edit::Array::new();
                    let mut changed = false;
                    for v in arr.iter() {
                        if let Some(s) = v.as_str() {
                            if s == dep_name || s.starts_with(&format!("{dep_name}/")) {
                                changed = true;
                                continue;
                            }
                        }
                        new_arr.push(v.clone());
                    }
                    if changed {
                        modifications.push((feat_name.to_string(), new_arr));
                    }
                }
            }
        }
    }
    // Phase 2: apply modifications
    if let Some(features) = doc.get_mut("features") {
        if let Some(table) = features.as_table_like_mut() {
            for (feat_name, new_arr) in modifications {
                table.insert(&feat_name, Item::Value(Value::Array(new_arr)));
            }
        }
    }
}

// Features don't need transformation since we use package aliasing
// e.g., `collections/test-support` still works because the dependency key is `collections`
// even though the actual package is `collections-unofficial`

fn remove_inspector_feature(doc: &mut DocumentMut) {
    // Remove from [features]
    if let Some(features) = doc.get_mut("features") {
        if let Some(table) = features.as_table_like_mut() {
            table.remove("inspector");
        }
    }

    // Remove from dependencies
    if let Some(deps) = doc.get_mut("dependencies") {
        if let Some(table) = deps.as_table_like_mut() {
            // Remove gpui dependency that's only used for inspector
            let dep_names: Vec<_> = table.iter().map(|(k, _)| k.to_string()).collect();
            for name in dep_names {
                if let Some(dep) = table.get(&name) {
                    if let Some(dep_table) = dep.as_table_like() {
                        // Check if this dep is only for inspector feature
                        if let Some(features) = dep_table.get("features") {
                            if features.as_array().is_some_and(|arr| {
                                arr.iter().any(|f| f.as_str() == Some("inspector"))
                                    && arr.len() == 1
                            }) {
                                table.remove(&name);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Add proptest as a dependency for crates that need it for tests.
/// This is needed because proptest is used by gpui and sum_tree tests but
/// may not be properly resolved from workspace dependencies.
fn add_proptest_dependency(doc: &mut DocumentMut) {
    // Add to [dependencies] as optional
    if let Some(deps) = doc.get_mut("dependencies") {
        if let Some(table) = deps.as_table_like_mut() {
            if !table.contains_key("proptest") {
                let mut dep = toml_edit::InlineTable::new();
                dep.insert("version", "1".into());
                dep.insert("optional", true.into());
                table.insert("proptest", Item::Value(Value::InlineTable(dep)));
            }
        }
    }

    // Add to [dev-dependencies]
    if let Some(deps) = doc.get_mut("dev-dependencies") {
        if let Some(table) = deps.as_table_like_mut() {
            if !table.contains_key("proptest") {
                let mut dep = toml_edit::InlineTable::new();
                dep.insert("version", "1".into());
                table.insert("proptest", Item::Value(Value::InlineTable(dep)));
            }
        }
    } else {
        // Create dev-dependencies section if it doesn't exist
        let mut dev_deps = toml_edit::Table::new();
        let mut dep = toml_edit::InlineTable::new();
        dep.insert("version", "1".into());
        dev_deps.insert("proptest", Item::Value(Value::InlineTable(dep)));
        doc.insert("dev-dependencies", Item::Table(dev_deps));
    }

    // Add dep:proptest to test-support feature
    if let Some(features) = doc.get_mut("features") {
        if let Some(table) = features.as_table_like_mut() {
            if let Some(test_support) = table.get_mut("test-support") {
                if let Some(arr) = test_support.as_array_mut() {
                    // Check if dep:proptest is already there
                    let has_proptest = arr.iter().any(|v| v.as_str() == Some("dep:proptest"));
                    if !has_proptest {
                        arr.push("dep:proptest");
                    }
                }
            }
        }
    }
}

/// Look up the latest version of a package on crates.io via `cargo search`.
/// Returns the version string (e.g. "29.0.1") or None if not found.
pub(crate) fn lookup_crates_io_version(package: &str) -> Option<String> {
    // Retry up to 3 times with backoff to handle crates.io rate limits
    for attempt in 0..3u64 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(5 * attempt));
        }
        let output = match Command::new("cargo")
            .args(["search", package, "--limit", "1"])
            .output()
        {
            Ok(o) => o,
            Err(_) => continue,
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        if stdout.is_empty() && !output.status.success() {
            // Likely rate limited, retry
            continue;
        }
        let prefix = format!("{package} = \"");
        for line in stdout.lines() {
            if line.starts_with(&prefix) {
                let after = &line[prefix.len()..];
                let version = after.split('"').next()?;
                return Some(version.to_string());
            }
        }
        // Got a valid response but package not found
        return None;
    }
    None
}

fn zed_tag_to_version(tag: &str) -> String {
    // Convert "v0.185.0" to "0.185.0"
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

fn write_metadata(output_dir: &Path, zed_tag: &str, zed_dir: &Path) -> Result<()> {
    // Get commit SHA
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(zed_dir)
        .output()?;
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let metadata = serde_json::json!({
        "zed_tag": zed_tag,
        "zed_commit": sha,
        "transformed_at": chrono::Utc::now().to_rfc3339(),
        "crates": CRATE_PUBLISH_ORDER,
    });

    let path = output_dir.join("transform-metadata.json");
    fs::write(path, serde_json::to_string_pretty(&metadata)?)?;

    Ok(())
}
