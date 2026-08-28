use anyhow::{bail, Context, Result};
use regex::Regex;
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use toml_edit::{DocumentMut, Item, Value};
use walkdir::WalkDir;

/// Crates to extract and publish, in topological order (dependencies first)
pub const CRATE_PUBLISH_ORDER: &[&str] = &[
    // Tier 1 - Leaf crates
    "gpui_util",
    "gpui_shared_string",
    "collections",
    "refineable/derive_refineable",
    "refineable",
    "tooling/perf",
    "path",
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
    "gpui_apple",
    "gpui_macos",
    "gpui_linux",
    "gpui_windows",
    "gpui_web",
    // Tier 5 - Facade
    "gpui_platform",
];

/// Crates zed only introduced partway through the release line we track.
///
/// `CRATE_PUBLISH_ORDER` is one flat list, but zed's crate roster moves between
/// releases: 1.17 split the Metal renderer out of `gpui_macos` into a new
/// `gpui_apple` crate, which does not exist at v1.16.1 or earlier. Listing such
/// a crate without recording when it appeared breaks every sync of an older tag
/// ("Crate not found"), and leaves `verify` waiting forever for a crate that
/// release can never contain.
///
/// Keyed by zed's directory name, valued by the first zed `(major, minor)` that
/// ships it. Add an entry here whenever you add a crate to the publish order
/// that isn't present in the whole supported tag range.
const CRATE_INTRODUCED_IN: &[(&str, (u32, u32))] = &[
    // zed 1.17 moved metal_renderer.rs out of gpui_macos into gpui_apple.
    ("gpui_apple", (1, 17)),
];

/// Pull `(major, minor)` out of a zed tag such as `v1.16.1` or `v1.17.0-pre`.
fn tag_major_minor(tag: &str) -> Option<(u32, u32)> {
    let numeric = tag.trim_start_matches('v').split(['-', '+']).next()?;
    let mut parts = numeric.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    Some((major, minor))
}

/// Does zed ship `crate_name` at `zed_tag`?
///
/// Only crates listed in [`CRATE_INTRODUCED_IN`] can answer `false`. Everything
/// else is assumed present at every tag, so an *unexpected* absence still fails
/// the transform loudly instead of quietly shrinking the set we publish.
pub fn crate_exists_at_tag(crate_name: &str, zed_tag: &str) -> bool {
    let Some(&(_, introduced)) = CRATE_INTRODUCED_IN.iter().find(|(name, _)| *name == crate_name)
    else {
        return true;
    };
    // An unparseable tag tells us nothing, so assume the crate is there and let
    // the caller report a real problem rather than skipping on a guess.
    tag_major_minor(zed_tag).is_none_or(|tag| tag >= introduced)
}

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

    // Transform each crate that this zed release actually has
    let mut transformed: Vec<&str> = Vec::new();
    for crate_path in CRATE_PUBLISH_ORDER {
        if !crate_exists_at_tag(crate_name_from_path(crate_path), zed_tag) {
            println!("Skipping: {crate_path} (not in zed {zed_tag})");
            continue;
        }
        println!("Transforming: {crate_path}");
        transform_crate(&zed_dir, &output_path, crate_path, &workspace_deps, zed_tag, use_local_deps)?;
        transformed.push(crate_path);
    }

    // Write metadata file
    write_metadata(&output_path, zed_tag, &zed_dir, &transformed)?;

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

    // Patch examples that reference external assets
    if crate_name == "gpui" {
        patch_text_example(&dest_dir)?;
    }

    // Copy in any assets the sources reach for outside their own crate.
    // Must run after patch_text_example, which matches on the original
    // include_bytes! text.
    vendor_external_assets(zed_dir, &src_dir, &dest_dir)?;

    // Transform Cargo.toml
    transform_cargo_toml(&dest_dir, output_dir, crate_name, workspace_deps, zed_tag, use_local_deps)?;

    // Patch source files for specific crates to remove inspector feature references
    if crate_name == "gpui_macros" || crate_name == "gpui" {
        patch_inspector_cfgs(&dest_dir)?;
    }

    // Patch gpui_macos to fix unnecessary unsafe block
    if crate_name == "gpui_macos" {
        patch_gpui_macos_source(&dest_dir)?;
    }

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

    // Note where upstream declared proptest as a dev-dependency before the
    // dependency pass strips it (it is a git-only pin). `add_proptest_dependency`
    // puts it back in the same table further down.
    let proptest_dev_dep_table = find_proptest_dev_dep_table(&doc);

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

    // For gpui, set lib name to "gpui" so users can `use gpui::...`
    // even though the package is named "gpui-unofficial"
    if original_name == "gpui" {
        // Update existing [lib] section or create new one
        if let Some(lib) = doc.get_mut("lib") {
            if let Some(table) = lib.as_table_like_mut() {
                table.insert("name", toml_edit::value("gpui"));
            }
        } else {
            let mut lib_table = toml_edit::Table::new();
            lib_table.insert("name", toml_edit::value("gpui"));
            lib_table.insert("path", toml_edit::value("src/lib.rs"));
            doc.insert("lib", Item::Table(lib_table));
        }

        // Add dev-dependency alias for gpui_platform so users can `use gpui_platform::...` in tests
        if let Some(dev_deps) = doc.get_mut("dev-dependencies") {
            if let Some(table) = dev_deps.as_table_like_mut() {
                let mut dep = toml_edit::InlineTable::new();
                dep.insert("package", "gpui-platform-gpui-unofficial".into());
                if use_local_deps {
                    dep.insert("path", "../gpui-platform-gpui-unofficial".into());
                } else {
                    dep.insert("version", version.clone().into());
                }
                table.insert("gpui_platform", Item::Value(Value::InlineTable(dep)));
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
        add_proptest_dependency(&mut doc, proptest_dev_dep_table.as_deref());
    }

    // Remove workspace lints (not supported for standalone crates)
    doc.remove("lints");

    // Add custom cfg lints for crates that need them
    add_custom_cfg_lints(&mut doc, original_name);

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
                                    // Try known fallbacks first, then lookup
                                    let version = known_git_dep_version(&pkg)
                                        .or_else(|| lookup_crates_io_version(&pkg));
                                    version.map(|ver| {
                                        println!("  Resolved git-only dep '{dep_name}' to crates.io {pkg}@{ver}");
                                        let mut t = toml_edit::InlineTable::new();
                                        t.insert("version", ver.into());
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

/// Does a `[features]` array entry activate `dep_name`?
///
/// Covers every form Cargo accepts: a bare `"dep_name"`, the explicit
/// `"dep:dep_name"`, and the `"dep_name/feature"` / `"dep_name?/feature"`
/// forms. Missing the `dep:` form leaves a dangling reference behind when the
/// dep itself is dropped, and Cargo refuses to parse the manifest at all:
/// "feature `x` includes `dep:y`, but `y` is not listed as a dependency".
fn feature_entry_refers_to_dep(entry: &str, dep_name: &str) -> bool {
    let entry = entry.strip_prefix("dep:").unwrap_or(entry);
    let name = entry.split('/').next().unwrap_or(entry);
    name.trim_end_matches('?') == dep_name
}

/// Remove all references to a dep from the `[features]` section.
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
                            if feature_entry_refers_to_dep(s, dep_name) {
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

/// Patch source files to remove inspector feature references.
/// Replaces various inspector cfg patterns with simpler versions.
fn patch_inspector_cfgs(crate_dir: &Path) -> Result<()> {
    let src_dir = crate_dir.join("src");
    if !src_dir.exists() {
        return Ok(());
    }

    for entry in WalkDir::new(&src_dir) {
        let entry = entry?;
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|e| e == "rs") {
            let content = fs::read_to_string(entry.path())?;

            // Replace various inspector cfg patterns
            let patched = content
                // Simple case: #[cfg(any(feature = "inspector", debug_assertions))]
                .replace(
                    "#[cfg(any(feature = \"inspector\", debug_assertions))]",
                    "#[cfg(debug_assertions)]"
                )
                // Negated case: #[cfg(not(any(feature = "inspector", debug_assertions)))]
                .replace(
                    "#[cfg(not(any(feature = \"inspector\", debug_assertions)))]",
                    "#[cfg(not(debug_assertions))]"
                )
                // Complex case with rust_analyzer: all(any(feature = "inspector", debug_assertions), not(rust_analyzer))
                .replace(
                    "all(any(feature = \"inspector\", debug_assertions), not(rust_analyzer))",
                    "all(debug_assertions, not(rust_analyzer))"
                );

            if patched != content {
                fs::write(entry.path(), patched)?;
            }
        }
    }

    Ok(())
}

/// Patch gpui_macos source to fix unnecessary unsafe block.
/// NSBeep() is now safe in newer objc bindings.
fn patch_gpui_macos_source(crate_dir: &Path) -> Result<()> {
    let window_rs = crate_dir.join("src/window.rs");
    if !window_rs.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&window_rs)?;

    // Remove unnecessary unsafe around NSBeep()
    let patched = content.replace(
        "unsafe { NSBeep() }",
        "NSBeep()"
    );

    if patched != content {
        fs::write(&window_rs, patched)?;
    }

    Ok(())
}

/// Patch text.rs example to remove external font dependency.
/// The example uses include_bytes! for a font file outside the crate.
fn patch_text_example(crate_dir: &Path) -> Result<()> {
    let text_rs = crate_dir.join("examples/text.rs");
    if !text_rs.exists() {
        return Ok(());
    }

    let content = fs::read_to_string(&text_rs)?;
    // Normalize line endings for cross-platform compatibility
    let content = content.replace("\r\n", "\n");

    // Remove the Cow import (no longer needed without include_bytes)
    let patched = content.replace(
        "    borrow::Cow,\n",
        ""
    );

    // Remove the font loading block
    let patched = patched.replace(
        r#"let fonts = [include_bytes!(
            "../../../assets/fonts/lilex/Lilex-Regular.ttf"
        )]
        .iter()
        .map(|b| Cow::Borrowed(&b[..]))
        .collect();

        _ = cx.text_system().add_fonts(fonts);

        "#,
        ""
    );

    if patched != content {
        fs::write(&text_rs, patched)?;
        println!("  Patched examples/text.rs (removed external font dependency)");
    }

    Ok(())
}

/// Matches `include_bytes!("…")` / `include_str!("…")`, including the
/// multi-line form where the path sits on its own line.
fn include_macro_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?s)include_(?:bytes|str)!\s*\(\s*"([^"]+)"\s*\)"#).unwrap()
    })
}

/// Resolve `.` and `..` without touching the filesystem.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c.as_os_str()),
        }
    }
    out
}

/// Path to `to_file` expressed relative to `from_dir`, using `/` separators so
/// the literal stays valid on Windows too.
fn relative_include_path(from_dir: &Path, to_file: &Path) -> String {
    let from: Vec<_> = from_dir.components().collect();
    let to: Vec<_> = to_file.components().collect();
    let shared = from
        .iter()
        .zip(to.iter())
        .take_while(|(a, b)| a == b)
        .count();

    let mut parts: Vec<String> = vec!["..".to_string(); from.len() - shared];
    parts.extend(
        to[shared..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join("/")
}

/// Where a vendored asset lands inside the extracted crate. Assets under zed's
/// repo root keep their layout (`assets/fonts/lilex/…`); anything else is
/// flattened into `vendored/`.
fn vendored_dest(zed_dir: &Path, asset: &Path, crate_dir: &Path) -> PathBuf {
    match asset.strip_prefix(normalize_lexically(zed_dir)) {
        Ok(relative) => crate_dir.join(relative),
        Err(_) => crate_dir.join("vendored").join(
            asset
                .file_name()
                .map(Path::new)
                .unwrap_or_else(|| Path::new("asset")),
        ),
    }
}

/// Copy the license sitting alongside a vendored asset. The bundled fonts are
/// OFL-licensed, and the license has to travel with them when we redistribute
/// them inside a published crate.
fn copy_sibling_licenses(asset: &Path, vendored: &Path) -> Result<()> {
    let (Some(src_dir), Some(dest_dir)) = (asset.parent(), vendored.parent()) else {
        return Ok(());
    };

    for entry in fs::read_dir(src_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let name = entry.file_name();
        let lower = name.to_string_lossy().to_lowercase();
        if !(lower.starts_with("license") || lower.starts_with("copying") || lower.starts_with("ofl"))
        {
            continue;
        }
        fs::create_dir_all(dest_dir)?;
        fs::copy(entry.path(), dest_dir.join(&name))?;
    }

    Ok(())
}

/// Copy assets that a crate's sources embed from outside their own directory,
/// and rewrite the include paths to point at the copies.
///
/// zed is a monorepo, so crates reach up to the repo-root `assets/` freely.
/// `gpui_web` embeds eight bundled fonts that way — a browser has no system
/// font source, so they are load-bearing library code, not test fixtures. Once
/// the crate is lifted out of the monorepo those paths dangle and it fails to
/// compile at all:
///
/// ```text
/// error: couldn't read `src/../../../assets/fonts/lilex/Lilex-Regular.ttf`
/// ```
///
/// gpui's and gpui_wgpu's test modules and benches reach out the same way; a
/// published crate can't read outside its own directory either, so vendoring
/// fixes those for downstream consumers too.
fn vendor_external_assets(zed_dir: &Path, src_dir: &Path, crate_dir: &Path) -> Result<()> {
    let crate_root = normalize_lexically(src_dir);
    let mut vendored_count = 0usize;

    for entry in WalkDir::new(crate_dir) {
        let entry = entry?;
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|e| e != "rs") {
            continue;
        }

        let content = fs::read_to_string(entry.path())?;
        if !content.contains("include_bytes!") && !content.contains("include_str!") {
            continue;
        }

        // Resolve includes against this file's *original* location in zed, so
        // the `..` hops land inside the monorepo.
        let relative = entry.path().strip_prefix(crate_dir)?;
        let Some(original_dir) = src_dir.join(relative).parent().map(Path::to_path_buf) else {
            continue;
        };
        let Some(file_dir) = entry.path().parent() else {
            continue;
        };

        let mut rewrites: Vec<(String, String)> = Vec::new();
        for caps in include_macro_re().captures_iter(&content) {
            let literal = &caps[1];
            let asset = normalize_lexically(&original_dir.join(literal));

            // Already inside the crate — it gets copied with everything else.
            if asset.starts_with(&crate_root) {
                continue;
            }
            // Generated at build time (e.g. concat!(env!("OUT_DIR"), …)) or
            // simply missing; leave it for the compiler to report.
            if !asset.is_file() {
                continue;
            }

            let dest = vendored_dest(zed_dir, &asset, crate_dir);
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&asset, &dest)
                .with_context(|| format!("Failed to vendor asset {}", asset.display()))?;
            copy_sibling_licenses(&asset, &dest)?;
            vendored_count += 1;

            rewrites.push((
                literal.to_string(),
                relative_include_path(file_dir, &dest),
            ));
        }

        if rewrites.is_empty() {
            continue;
        }

        let mut patched = content;
        for (old, new) in rewrites {
            patched = patched.replace(&format!("\"{old}\""), &format!("\"{new}\""));
        }
        fs::write(entry.path(), patched)?;
    }

    if vendored_count > 0 {
        println!("  Vendored {vendored_count} external asset(s)");
    }

    Ok(())
}

/// Find the `target.'<cfg>'` key under which proptest is declared as a
/// dev-dependency, if it is declared under one at all.
///
/// Returns `None` when proptest sits in the plain `[dev-dependencies]` table
/// (or is absent). zed gates gpui's proptest dev-dependency behind
/// `cfg(not(target_family = "wasm"))` because proptest pulls in `rusty-fork`
/// and `wait-timeout`, which are process-based and do not build for wasm.
fn find_proptest_dev_dep_table(doc: &DocumentMut) -> Option<String> {
    let targets = doc.get("target")?.as_table_like()?;
    targets.iter().find_map(|(cfg, item)| {
        let declares_proptest = item
            .as_table_like()?
            .get("dev-dependencies")?
            .as_table_like()?
            .contains_key("proptest");
        declares_proptest.then(|| cfg.to_string())
    })
}

/// Get (creating if needed) the dev-dependencies table proptest belongs in.
///
/// `target_cfg` is the `cfg(...)` predicate proptest was gated behind upstream,
/// or `None` for the plain `[dev-dependencies]` table.
fn dev_dep_table<'a>(
    doc: &'a mut DocumentMut,
    target_cfg: Option<&str>,
) -> Option<&'a mut dyn toml_edit::TableLike> {
    let Some(cfg) = target_cfg else {
        return doc
            .entry("dev-dependencies")
            .or_insert_with(|| Item::Table(toml_edit::Table::new()))
            .as_table_like_mut();
    };

    let targets = doc
        .entry("target")
        .or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_like_mut()?;
    let target = targets
        .entry(cfg)
        .or_insert_with(|| {
            let mut table = toml_edit::Table::new();
            table.set_implicit(true);
            Item::Table(table)
        })
        .as_table_like_mut()?;
    target
        .entry("dev-dependencies")
        .or_insert_with(|| Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
}

/// Add proptest as a dependency for crates that need it for tests.
/// This is needed because proptest is used by gpui and sum_tree tests but
/// may not be properly resolved from workspace dependencies.
///
/// `dev_dep_target_cfg` is the target gate upstream declared the dev-dependency
/// under, as returned by [`find_proptest_dev_dep_table`].
fn add_proptest_dependency(doc: &mut DocumentMut, dev_dep_target_cfg: Option<&str>) {
    // Add to [dependencies] as optional
    if let Some(deps) = doc.get_mut("dependencies") {
        if let Some(table) = deps.as_table_like_mut() {
            if !table.contains_key("proptest") {
                let mut dep = toml_edit::InlineTable::new();
                dep.insert("version", "1".into());
                dep.insert("optional", true.into());
                let mut features = toml_edit::Array::new();
                features.push("attr-macro");
                dep.insert("features", features.into());
                table.insert("proptest", Item::Value(Value::InlineTable(dep)));
            }
        }
    }

    // Add to dev-dependencies, in whichever table upstream declared it in.
    // Restoring the original table matters: dropping gpui's proptest into the
    // untargeted [dev-dependencies] makes it apply to wasm as well, and
    // building gpui's examples for wasm32 then fails compiling `wait-timeout`.
    if let Some(table) = dev_dep_table(doc, dev_dep_target_cfg) {
        if !table.contains_key("proptest") {
            let mut dep = toml_edit::InlineTable::new();
            dep.insert("version", "1".into());
            let mut features = toml_edit::Array::new();
            features.push("attr-macro");
            dep.insert("features", features.into());
            table.insert("proptest", Item::Value(Value::InlineTable(dep)));
        }
    }

    // Declare an explicit `proptest` feature and enable it from `test-support`.
    //
    // gpui gates code behind `#[cfg(feature = "proptest")]`. Upstream relies on
    // Cargo's *implicit* feature for the optional `proptest` dependency, but our
    // transform strips proptest (a git-only pin) and re-adds it here. The moment
    // any feature references the dep with `dep:proptest` syntax, Cargo stops
    // creating that implicit feature -- which turns `cfg(feature = "proptest")`
    // into an "unexpected cfg" and fails the sync check under `-Dwarnings`.
    // Declaring the feature explicitly keeps the gate valid regardless of how
    // the dependency is referenced.
    if let Some(features) = doc.get_mut("features") {
        if let Some(table) = features.as_table_like_mut() {
            // Explicit `proptest = ["dep:proptest"]` feature.
            if !table.contains_key("proptest") {
                let mut arr = toml_edit::Array::new();
                arr.push("dep:proptest");
                table.insert("proptest", Item::Value(Value::Array(arr)));
            }
            // Enable proptest from `test-support` using the feature name (matches
            // upstream, which lists a bare `proptest`). Referencing the feature
            // -- not `dep:proptest` -- avoids re-suppressing the implicit feature.
            if let Some(test_support) = table.get_mut("test-support") {
                if let Some(arr) = test_support.as_array_mut() {
                    let already = arr
                        .iter()
                        .any(|v| matches!(v.as_str(), Some("proptest") | Some("dep:proptest")));
                    if !already {
                        arr.push("proptest");
                    }
                }
            }
        }
    }
}

/// Add lints configuration for crates that use custom cfg attributes.
fn add_custom_cfg_lints(doc: &mut DocumentMut, crate_name: &str) {
    let check_cfgs: &[&str] = match crate_name {
        "ztracing" => &["cfg(ztracing)", "cfg(ztracing_with_memory)"],
        "util_macros" => &["cfg(perf_enabled)"],
        "gpui" => &["cfg(rust_analyzer)"],
        // objc crate macros use cargo-clippy cfg. `gpui_apple` holds the Metal
        // renderer that `gpui_macos` used to own, and it calls `msg_send!` too.
        "gpui_apple" | "gpui_macos" => &["cfg(feature, values(\"cargo-clippy\"))"],
        // nightly_coverage feature for code coverage
        "gpui_linux" => &["cfg(feature, values(\"nightly_coverage\"))"],
        _ => return, // No custom cfgs needed
    };

    // Create [lints.rust] with check-cfg for custom attributes
    let mut check_cfg_arr = toml_edit::Array::new();
    for cfg in check_cfgs {
        check_cfg_arr.push(*cfg);
    }

    let mut unexpected_cfgs = toml_edit::InlineTable::new();
    unexpected_cfgs.insert("level", "warn".into());
    unexpected_cfgs.insert("check-cfg", toml_edit::Value::Array(check_cfg_arr));

    let mut rust_lints = toml_edit::InlineTable::new();
    rust_lints.insert("unexpected_cfgs", toml_edit::Value::InlineTable(unexpected_cfgs));

    // gpui_apple's Metal renderer still uses `cocoa::foundation::{NSSize, NSUInteger}`,
    // which upstream `cocoa` deprecated in favor of `objc2-foundation`. zed hasn't
    // migrated this file yet, so under our `-Dwarnings` build these deprecation
    // warnings become hard errors (only reached once something actually compiles
    // the crate, e.g. `cargo build --examples`, not the lighter `cargo check`).
    if crate_name == "gpui_apple" {
        rust_lints.insert("deprecated", "allow".into());
    }

    let mut lints_table = toml_edit::Table::new();
    lints_table.insert("rust", Item::Value(toml_edit::Value::InlineTable(rust_lints)));

    doc.insert("lints", Item::Table(lints_table));
}

/// Known git-only deps that have crates.io equivalents.
/// Fallback when cargo search fails (e.g., rate limits, network issues).
fn known_git_dep_version(package: &str) -> Option<String> {
    match package {
        "wgpu" => Some("29.0.1".to_string()),
        _ => None,
    }
}

/// Look up the latest version of a package on crates.io via the crates.io API.
/// Returns the version string (e.g. "29.0.1") or None if not found.
pub(crate) fn lookup_crates_io_version(package: &str) -> Option<String> {
    let url = format!("https://crates.io/api/v1/crates/{package}");
    
    // Retry up to 3 times with backoff to handle transient failures
    for attempt in 0..3 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_secs(2 * attempt as u64));
        }
        
        match reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .user_agent("gpui-unofficial-transform/0.1")
            .build()
            .and_then(|client| client.get(&url).send())
        {
            Ok(response) => {
                if response.status().is_success() {
                    // Use serde_json::from_reader instead of response.json()
                    match serde_json::from_reader::<_, serde_json::Value>(response) {
                        Ok(json) => {
                            if let Some(versions) = json.get("versions").and_then(|v| v.as_array()) {
                                // The API returns versions sorted with latest first
                                for version_info in versions {
                                    if let Some(version) = version_info.get("num").and_then(|v| v.as_str()) {
                                        // Check if it's a stable release (no pre-release)
                                        // If all are pre-release, return the first one
                                        let num = version.to_string();
                                        if !num.contains('-') {
                                            return Some(num);
                                        }
                                    }
                                }
                                // If only pre-releases, return the first one
                                if let Some(version_info) = versions.first() {
                                    if let Some(version) = version_info.get("num").and_then(|v| v.as_str()) {
                                        return Some(version.to_string());
                                    }
                                }
                            }
                            return None;
                        }
                        Err(e) => {
                            if attempt == 2 {
                                eprintln!("Failed to parse JSON for {}: {}", package, e);
                                return None;
                            }
                            continue;
                        }
                    }
                }
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    return None;
                }
                if attempt == 2 {
                    return None;
                }
                continue;
            }
            Err(e) => {
                if attempt == 2 {
                    eprintln!("Failed to lookup crate {}: {}", package, e);
                    return None;
                }
                continue;
            }
        }
    }
    None
}

fn zed_tag_to_version(tag: &str) -> String {
    // Convert "v0.185.0" to "0.185.0"
    tag.strip_prefix('v').unwrap_or(tag).to_string()
}

fn write_metadata(
    output_dir: &Path,
    zed_tag: &str,
    zed_dir: &Path,
    crates: &[&str],
) -> Result<()> {
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
        // The crates this tag actually produced, not the full publish order —
        // older tags legitimately lack some of it (see CRATE_INTRODUCED_IN).
        "crates": crates,
    });

    let path = output_dir.join("transform-metadata.json");
    fs::write(path, serde_json::to_string_pretty(&metadata)?)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The v1.16.1 sync failure: `gpui_apple` was added to the publish order
    /// for zed 1.17, but the scheduled run targets zed's latest *stable*
    /// release, which was still v1.16.1 — a tag with no `crates/gpui_apple`.
    /// The transform bailed with "Crate not found" on all four platforms.
    #[test]
    fn crates_are_only_expected_from_the_release_that_introduced_them() {
        for tag in ["v1.17.0-pre", "v1.17.0", "v1.18.2", "v2.0.0"] {
            assert!(
                crate_exists_at_tag("gpui_apple", tag),
                "gpui_apple exists from zed 1.17 on, including {tag}"
            );
        }
        for tag in ["v1.16.1", "v1.16.0-pre", "v1.15.0", "v0.185.0"] {
            assert!(
                !crate_exists_at_tag("gpui_apple", tag),
                "gpui_apple does not exist at {tag}"
            );
        }
    }

    /// Only crates listed in `CRATE_INTRODUCED_IN` may be skipped. Everything
    /// else must still fail loudly when it goes missing, so an upstream rename
    /// can't silently shrink the set of crates we publish.
    #[test]
    fn unlisted_crates_are_expected_at_every_tag() {
        for entry in CRATE_PUBLISH_ORDER {
            let name = crate_name_from_path(entry);
            if CRATE_INTRODUCED_IN.iter().any(|(n, _)| *n == name) {
                continue;
            }
            assert!(
                crate_exists_at_tag(name, "v1.0.0"),
                "{name} is not version-gated, so it must be expected everywhere"
            );
        }
        // An unparseable tag must not be read as "missing" either.
        assert!(crate_exists_at_tag("gpui_apple", "nightly"));
    }

    #[test]
    fn parses_major_minor_from_stable_and_preview_tags() {
        assert_eq!(tag_major_minor("v1.16.1"), Some((1, 16)));
        assert_eq!(tag_major_minor("1.16.1"), Some((1, 16)));
        assert_eq!(tag_major_minor("v1.17.0-pre"), Some((1, 17)));
        assert_eq!(tag_major_minor("v0.185.0"), Some((0, 185)));
        assert_eq!(tag_major_minor("main"), None);
        // Ordering has to be numeric, not lexicographic: "1.9" < "1.17".
        assert!(tag_major_minor("v1.9.0") < tag_major_minor("v1.17.0"));
    }

    /// zed's `http_client` declares `async-tar` as an optional *git-only*
    /// workspace dep, so the transform drops it — but `github-download` still
    /// activates it as `"dep:async-tar"`. Leaving that entry behind makes Cargo
    /// reject the manifest outright ("feature `github-download` includes
    /// `dep:async-tar`, but `async-tar` is not listed as a dependency"), which
    /// fails `cargo check` on every platform.
    #[test]
    fn removes_dep_prefixed_feature_entries() {
        let mut doc: DocumentMut = r#"
[features]
github-download = ["dep:async-fs", "dep:async-tar", "dep:sha2"]
"#
        .parse()
        .unwrap();

        remove_dep_from_features(&mut doc, "async-tar");

        let entries: Vec<String> = doc["features"]["github-download"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert_eq!(entries, vec!["dep:async-fs", "dep:sha2"]);
    }

    /// The other activation forms must keep working, and a dep whose name is a
    /// prefix of another must not drag its neighbour out with it.
    #[test]
    fn matches_every_activation_form_without_overreaching() {
        for entry in ["async-tar", "dep:async-tar", "async-tar/unstable", "async-tar?/unstable"] {
            assert!(
                feature_entry_refers_to_dep(entry, "async-tar"),
                "{entry} should match async-tar"
            );
        }
        for entry in ["dep:async-tar-extra", "async-fs", "dep:async-fs", "async-tar-extra/x"] {
            assert!(
                !feature_entry_refers_to_dep(entry, "async-tar"),
                "{entry} should not match async-tar"
            );
        }
    }

    /// The exact shape that broke the v1.15.1 sync: `gpui_web/src/platform.rs`
    /// embeds fonts from zed's repo-root `assets/`, three levels above the
    /// crate. They must land inside the crate and the literal must be rewritten
    /// to reach them, or the crate doesn't compile once it leaves the monorepo.
    #[test]
    fn vendors_assets_referenced_from_outside_the_crate() {
        let zed = tempfile::tempdir().unwrap();
        let zed_dir = zed.path();

        let font_dir = zed_dir.join("assets/fonts/lilex");
        fs::create_dir_all(&font_dir).unwrap();
        fs::write(font_dir.join("Lilex-Regular.ttf"), b"ttf-bytes").unwrap();
        fs::write(font_dir.join("OFL.txt"), b"font license").unwrap();

        let src_dir = zed_dir.join("crates/gpui_web");
        fs::create_dir_all(src_dir.join("src")).unwrap();
        let source = r#"static BUNDLED_FONTS: &[&[u8]] = &[
    include_bytes!("../../../assets/fonts/lilex/Lilex-Regular.ttf"),
];
"#;
        fs::write(src_dir.join("src/platform.rs"), source).unwrap();

        let out = tempfile::tempdir().unwrap();
        let crate_dir = out.path().join("gpui-web-gpui-unofficial");
        copy_dir_recursive(&src_dir, &crate_dir).unwrap();

        vendor_external_assets(zed_dir, &src_dir, &crate_dir).unwrap();

        // The font — and its license — travel with the crate.
        let vendored = crate_dir.join("assets/fonts/lilex/Lilex-Regular.ttf");
        assert_eq!(fs::read(&vendored).unwrap(), b"ttf-bytes");
        assert!(
            crate_dir.join("assets/fonts/lilex/OFL.txt").is_file(),
            "the OFL license must ship alongside the redistributed font"
        );

        // The include now points at the vendored copy, relative to src/.
        let patched = fs::read_to_string(crate_dir.join("src/platform.rs")).unwrap();
        assert!(
            patched.contains(r#"include_bytes!("../assets/fonts/lilex/Lilex-Regular.ttf")"#),
            "include path should be rewritten, got:\n{patched}"
        );
        assert!(!patched.contains("../../../assets"));
    }

    /// An include that already resolves inside the crate is copied along with
    /// the rest of the tree, so it must be left exactly as it is.
    #[test]
    fn leaves_includes_that_stay_inside_the_crate_alone() {
        let zed = tempfile::tempdir().unwrap();
        let src_dir = zed.path().join("crates/gpui");
        fs::create_dir_all(src_dir.join("src")).unwrap();
        fs::create_dir_all(src_dir.join("examples/image")).unwrap();
        fs::write(src_dir.join("examples/image/photo.jpg"), b"jpg").unwrap();
        let source = r#"const P: &[u8] = include_bytes!("../examples/image/photo.jpg");"#;
        fs::write(src_dir.join("src/platform.rs"), source).unwrap();

        let out = tempfile::tempdir().unwrap();
        let crate_dir = out.path().join("gpui-unofficial");
        copy_dir_recursive(&src_dir, &crate_dir).unwrap();

        vendor_external_assets(zed.path(), &src_dir, &crate_dir).unwrap();

        assert_eq!(
            fs::read_to_string(crate_dir.join("src/platform.rs")).unwrap(),
            source
        );
        assert!(!crate_dir.join("vendored").exists());
    }

    /// `include_bytes!(concat!(env!("OUT_DIR"), …))` and other non-literal or
    /// missing paths must not abort the transform — gpui_macos builds its
    /// metallib that way.
    #[test]
    fn ignores_includes_it_cannot_resolve() {
        let zed = tempfile::tempdir().unwrap();
        let src_dir = zed.path().join("crates/gpui_macos");
        fs::create_dir_all(src_dir.join("src")).unwrap();
        let source = r#"const S: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/shaders.metallib"));
const M: &[u8] = include_bytes!("../../../assets/does-not-exist.bin");
"#;
        fs::write(src_dir.join("src/renderer.rs"), source).unwrap();

        let out = tempfile::tempdir().unwrap();
        let crate_dir = out.path().join("gpui-macos-gpui-unofficial");
        copy_dir_recursive(&src_dir, &crate_dir).unwrap();

        vendor_external_assets(zed.path(), &src_dir, &crate_dir).unwrap();

        assert_eq!(
            fs::read_to_string(crate_dir.join("src/renderer.rs")).unwrap(),
            source
        );
    }

    #[test]
    fn builds_relative_include_paths_with_forward_slashes() {
        let root = Path::new("/out/gpui-web-gpui-unofficial");
        assert_eq!(
            relative_include_path(
                &root.join("src"),
                &root.join("assets/fonts/lilex/Lilex-Regular.ttf")
            ),
            "../assets/fonts/lilex/Lilex-Regular.ttf"
        );
        // Deeper source directories need more hops back up.
        assert_eq!(
            relative_include_path(&root.join("src/platform/web"), &root.join("assets/f.ttf")),
            "../../../assets/f.ttf"
        );
    }

    /// proptest pulls in `rusty-fork` -> `wait-timeout`, which does not build
    /// for wasm32, so zed confines gpui's proptest dev-dependency to
    /// `cfg(not(target_family = "wasm"))`. Re-adding it to the untargeted
    /// `[dev-dependencies]` breaks `cargo build --examples` for wasm32.
    #[test]
    fn keeps_proptest_dev_dep_in_the_target_table_upstream_used() {
        // Mirrors gpui's Cargo.toml: proptest gated behind non-wasm, alongside
        // the other deps that share the gate.
        let gpui = r#"
[dependencies]
collections = { version = "1" }

[dev-dependencies]
rand = { version = "0.9" }

[target.'cfg(not(target_family = "wasm"))'.dev-dependencies]
http_client = { version = "1" }
proptest = { version = "1" }

[features]
test-support = ["collections/test-support"]
"#;
        let mut doc: DocumentMut = gpui.parse().unwrap();
        let cfg = find_proptest_dev_dep_table(&doc).expect("proptest is gated upstream");
        assert_eq!(cfg, r#"cfg(not(target_family = "wasm"))"#);

        // Simulate the dependency pass dropping the git-only proptest pin.
        doc["target"][&cfg]["dev-dependencies"]
            .as_table_like_mut()
            .unwrap()
            .remove("proptest");

        add_proptest_dependency(&mut doc, Some(&cfg));

        assert!(
            doc["target"][&cfg]["dev-dependencies"]
                .as_table_like()
                .unwrap()
                .contains_key("proptest"),
            "proptest should be restored to the non-wasm dev-dependency table"
        );
        assert!(
            !doc["dev-dependencies"]
                .as_table_like()
                .unwrap()
                .contains_key("proptest"),
            "proptest must not leak into the untargeted dev-dependencies, or it \
             applies to wasm too"
        );
        // The manifest still parses, and the gate is written the way Cargo
        // expects rather than as a mangled key.
        assert!(
            doc.to_string()
                .contains(r#"[target.'cfg(not(target_family = "wasm"))'.dev-dependencies]"#),
            "target table header should round-trip: {doc}"
        );
    }

    /// The gate key contains double quotes, so creating the target table from
    /// scratch has to emit a header Cargo can still parse.
    #[test]
    fn creates_a_reparseable_target_table_when_one_is_missing() {
        let mut doc: DocumentMut = "[dependencies]\ncollections = { version = \"1\" }\n"
            .parse()
            .unwrap();
        let cfg = r#"cfg(not(target_family = "wasm"))"#;

        add_proptest_dependency(&mut doc, Some(cfg));

        let rendered = doc.to_string();
        let reparsed: DocumentMut = rendered
            .parse()
            .unwrap_or_else(|e| panic!("generated manifest must parse: {e}\n{rendered}"));
        assert!(
            reparsed["target"][cfg]["dev-dependencies"]
                .as_table_like()
                .unwrap()
                .contains_key("proptest")
        );
    }

    /// sum_tree declares proptest in the plain `[dev-dependencies]` table, with
    /// no target gate. It should stay there.
    #[test]
    fn leaves_untargeted_proptest_dev_deps_untargeted() {
        let mut doc: DocumentMut = r#"
[dependencies]
heapless = { version = "0.9" }

[dev-dependencies]
rand = { version = "0.9" }
proptest = { version = "1" }

[features]
test-support = ["proptest"]
"#
        .parse()
        .unwrap();
        assert_eq!(find_proptest_dev_dep_table(&doc), None);

        doc["dev-dependencies"].as_table_like_mut().unwrap().remove("proptest");
        add_proptest_dependency(&mut doc, None);

        assert!(
            doc["dev-dependencies"]
                .as_table_like()
                .unwrap()
                .contains_key("proptest")
        );
        assert!(doc.get("target").is_none(), "no target table should be invented");
    }

    /// After `transform_dependencies` strips the git-only optional `proptest`
    /// dep and `remove_dep_from_features` removes the bare `proptest` from
    /// `test-support`, `add_proptest_dependency` must re-add proptest in a way
    /// that keeps `#[cfg(feature = "proptest")]` a *known* cfg. That requires an
    /// explicit `proptest` feature; otherwise the `dep:proptest` reference
    /// suppresses Cargo's implicit feature and the gpui sync check fails under
    /// `-Dwarnings` with `unexpected cfg condition value: proptest`.
    #[test]
    fn declares_explicit_proptest_feature_so_cfg_gate_is_known() {
        // Mirrors the post-removal state of gpui's Cargo.toml.
        let mut doc: DocumentMut = r#"
[dependencies]
collections = { version = "1" }

[features]
test-support = ["collections/test-support", "rand"]
"#
        .parse()
        .unwrap();

        add_proptest_dependency(&mut doc, None);

        // Optional proptest dependency restored with the attr-macro feature.
        let dep = doc["dependencies"]["proptest"].as_inline_table().unwrap();
        assert_eq!(dep.get("optional").and_then(|v| v.as_bool()), Some(true));
        assert!(
            dep.get("features")
                .and_then(|v| v.as_array())
                .map(|a| a.iter().any(|v| v.as_str() == Some("attr-macro")))
                .unwrap_or(false),
            "proptest dep should enable the attr-macro feature"
        );

        // An explicit `proptest = ["dep:proptest"]` feature must exist so the
        // cfg gate is a known feature under -Dwarnings.
        let proptest_feat = doc["features"]["proptest"]
            .as_array()
            .expect("explicit `proptest` feature must be declared");
        assert!(
            proptest_feat
                .iter()
                .any(|v| v.as_str() == Some("dep:proptest")),
            "`proptest` feature should enable the optional dep via dep:proptest"
        );

        // test-support enables it by feature name, NOT `dep:proptest` (which
        // would re-suppress the implicit feature we're trying to preserve).
        let ts: Vec<String> = doc["features"]["test-support"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert!(
            ts.contains(&"proptest".to_string()),
            "test-support should enable the proptest feature by name"
        );
        assert!(
            !ts.contains(&"dep:proptest".to_string()),
            "test-support must not reference dep:proptest directly"
        );
    }

    /// `objc`'s `sel_impl!` -- reached through every `msg_send!` -- expands to
    /// `#[cfg_attr(feature = "cargo-clippy", …)]`. zed silences that repo-wide
    /// with `unexpected_cfgs = { level = "allow" }` in `[workspace.lints]`,
    /// which the transform strips, so each extracted crate that calls
    /// `msg_send!` needs the cfg declared here instead. The sync workflow sets
    /// `RUSTFLAGS: -Dwarnings`, so a missing entry is a build failure.
    ///
    /// zed 1.17 moved the Metal renderer out of `gpui_macos` into the new
    /// `gpui_apple` crate, taking the `msg_send!` calls with it.
    #[test]
    fn allows_cargo_clippy_cfg_for_every_objc_crate() {
        for crate_name in ["gpui_apple", "gpui_macos"] {
            let mut doc = DocumentMut::new();
            add_custom_cfg_lints(&mut doc, crate_name);

            let check_cfg: Vec<String> = doc["lints"]["rust"]["unexpected_cfgs"]["check-cfg"]
                .as_array()
                .unwrap_or_else(|| panic!("{crate_name} declares no check-cfg list"))
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect();

            assert!(
                check_cfg.contains(&r#"cfg(feature, values("cargo-clippy"))"#.to_string()),
                "{crate_name} calls msg_send! and must allow the cargo-clippy cfg, got {check_cfg:?}"
            );
        }
    }

    /// `gpui_apple`'s Metal renderer (`metal_renderer.rs`) still imports
    /// `cocoa::foundation::{NSSize, NSUInteger}`, which the `cocoa` crate has
    /// deprecated in favor of `objc2-foundation`. zed hasn't migrated this file
    /// yet, so under the sync workflow's `RUSTFLAGS: -Dwarnings` these
    /// deprecation warnings become build errors -- but only once something
    /// actually compiles the crate (e.g. `cargo build --examples`); the lighter
    /// `cargo check` on just `gpui-unofficial` doesn't pull `gpui_apple` in, so
    /// it stays green while example builds fail.
    #[test]
    fn allows_deprecated_cocoa_apis_in_gpui_apple_only() {
        let mut doc = DocumentMut::new();
        add_custom_cfg_lints(&mut doc, "gpui_apple");
        assert_eq!(
            doc["lints"]["rust"]["deprecated"].as_str(),
            Some("allow"),
            "gpui_apple must allow deprecated lints for its still-unmigrated cocoa usage"
        );

        // gpui_macos no longer owns the Metal renderer (zed 1.17 moved it to
        // gpui_apple), so it shouldn't get a blanket deprecated-allow it doesn't need.
        let mut doc = DocumentMut::new();
        add_custom_cfg_lints(&mut doc, "gpui_macos");
        assert!(
            doc["lints"]["rust"].get("deprecated").is_none(),
            "gpui_macos should not need the deprecated allow"
        );
    }
}

