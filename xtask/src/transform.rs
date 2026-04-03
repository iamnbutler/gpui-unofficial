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
    "ztracing_macro",
    "ztracing",
    "zlog",
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
    // Convert snake_case to kebab-case and add suffix
    let kebab = name.replace('_', "-");
    format!("{kebab}-unofficial")
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

    // Transform dependencies
    transform_dependencies(&mut doc, "dependencies", workspace_deps, &version, output_dir, use_local_deps)?;
    transform_dependencies(&mut doc, "dev-dependencies", workspace_deps, &version, output_dir, use_local_deps)?;
    transform_dependencies(&mut doc, "build-dependencies", workspace_deps, &version, output_dir, use_local_deps)?;

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
                                    transform_dependencies(&mut temp_doc, dep_section, workspace_deps, &version, output_dir, use_local_deps)?;
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

    // Remove inspector feature from gpui_macros and gpui
    if original_name == "gpui_macros" || original_name == "gpui" {
        remove_inspector_feature(&mut doc);
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
fn is_internal_crate(dep_name: &str) -> bool {
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
) -> Result<()> {
    let Some(deps) = doc.get_mut(section) else {
        return Ok(());
    };

    let Some(deps_table) = deps.as_table_like_mut() else {
        return Ok(());
    };

    let dep_names: Vec<_> = deps_table.iter().map(|(k, _)| k.to_string()).collect();

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
                        let resolved = resolve_workspace_dep(workspace_dep, dep)?;
                        deps_table.insert(&dep_name, resolved);
                    }
                }
            }
        }
    }

    Ok(())
}

fn resolve_workspace_dep(workspace_def: &Item, usage: &Item) -> Result<Item> {
    // Get the base definition from workspace
    let mut result = if let Some(version) = workspace_def.as_str() {
        // Simple version string
        let mut table = toml_edit::InlineTable::new();
        table.insert("version", version.into());
        Item::Value(Value::InlineTable(table))
    } else if let Some(table) = workspace_def.as_table_like() {
        // Table with version, git, or other fields
        let mut new_table = toml_edit::InlineTable::new();

        // Copy version if present
        if let Some(version) = table.get("version").and_then(|v| v.as_str()) {
            new_table.insert("version", version.into());
        }

        // Copy package rename if present
        if let Some(pkg) = table.get("package").and_then(|v| v.as_str()) {
            new_table.insert("package", pkg.into());
        }

        // Copy git fields if present
        if let Some(git) = table.get("git").and_then(|v| v.as_str()) {
            new_table.insert("git", git.into());
        }
        if let Some(rev) = table.get("rev").and_then(|v| v.as_str()) {
            new_table.insert("rev", rev.into());
        }
        if let Some(branch) = table.get("branch").and_then(|v| v.as_str()) {
            new_table.insert("branch", branch.into());
        }
        if let Some(tag) = table.get("tag").and_then(|v| v.as_str()) {
            new_table.insert("tag", tag.into());
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

    Ok(result)
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

// No source file transformation needed - we use Cargo.toml package aliasing instead
// e.g., collections = { package = "collections-unofficial", version = "..." }
// This lets code keep using `use collections::...` while pulling the unofficial crate

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
