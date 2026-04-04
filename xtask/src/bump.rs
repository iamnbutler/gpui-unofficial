use anyhow::{bail, Result};
use std::fs;
use std::path::Path;
use toml_edit::DocumentMut;

use crate::transform::{CRATE_PUBLISH_ORDER, crate_name_from_path, unofficial_name};

pub fn run(crates_dir: &str, new_version: &str) -> Result<()> {
    // Validate semver format
    let parts: Vec<&str> = new_version.split('.').collect();
    if parts.len() != 3 || parts.iter().any(|p| p.parse::<u64>().is_err()) {
        bail!("Version must be semver (x.y.z), got: {new_version}");
    }

    let crates_path = Path::new(crates_dir);
    if !crates_path.exists() {
        bail!("Crates directory not found: {crates_dir}");
    }

    println!("Bumping all crates to version {new_version}");

    let mut updated = 0;
    for crate_entry in CRATE_PUBLISH_ORDER {
        let crate_name = crate_name_from_path(crate_entry);
        let pkg_name = unofficial_name(crate_name);
        let cargo_toml_path = crates_path.join(&pkg_name).join("Cargo.toml");

        if !cargo_toml_path.exists() {
            println!("  Skipping {pkg_name} (not found)");
            continue;
        }

        let content = fs::read_to_string(&cargo_toml_path)?;
        let mut doc: DocumentMut = content.parse()?;
        let old_version = doc
            .get("package")
            .and_then(|p| p.get("version"))
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Update package version
        if let Some(package) = doc.get_mut("package") {
            if let Some(table) = package.as_table_like_mut() {
                table.insert("version", toml_edit::value(new_version));
            }
        }

        // Update internal dependency versions in all sections
        update_internal_dep_versions(&mut doc, "dependencies", new_version);
        update_internal_dep_versions(&mut doc, "dev-dependencies", new_version);
        update_internal_dep_versions(&mut doc, "build-dependencies", new_version);

        // Handle target-specific sections
        let target_names: Vec<String> = doc
            .get("target")
            .and_then(|t| t.as_table_like())
            .map(|t| t.iter().map(|(k, _)| k.to_string()).collect())
            .unwrap_or_default();

        for target_name in &target_names {
            for section in ["dependencies", "dev-dependencies", "build-dependencies"] {
                let has_section = doc
                    .get("target")
                    .and_then(|t| t.get(target_name))
                    .and_then(|s| s.get(section))
                    .is_some();

                if has_section {
                    // Work on a temporary doc to avoid borrow issues
                    let deps_item = doc["target"][target_name.as_str()][section].clone();
                    let mut temp_doc = DocumentMut::new();
                    temp_doc.insert(section, deps_item);
                    update_internal_dep_versions(&mut temp_doc, section, new_version);
                    if let Some(new_deps) = temp_doc.get(section).cloned() {
                        if let Some(target_section) = doc
                            .get_mut("target")
                            .and_then(|t| t.get_mut(target_name.as_str()))
                            .and_then(|s| s.as_table_like_mut())
                        {
                            target_section.insert(section, new_deps);
                        }
                    }
                }
            }
        }

        fs::write(&cargo_toml_path, doc.to_string())?;
        println!("  {pkg_name}: {old_version} -> {new_version}");
        updated += 1;
    }

    // Update transform-metadata.json if it exists
    let metadata_path = crates_path.join("transform-metadata.json");
    if metadata_path.exists() {
        let content = fs::read_to_string(&metadata_path)?;
        if let Ok(mut meta) = serde_json::from_str::<serde_json::Value>(&content) {
            meta["bumped_to"] = serde_json::Value::String(new_version.to_string());
            fs::write(&metadata_path, serde_json::to_string_pretty(&meta)?)?;
            println!("  Updated transform-metadata.json");
        }
    }

    println!("\nBumped {updated} crates to {new_version}");
    Ok(())
}

fn update_internal_dep_versions(doc: &mut DocumentMut, section: &str, new_version: &str) {
    let Some(deps) = doc.get_mut(section) else {
        return;
    };
    let Some(table) = deps.as_table_like_mut() else {
        return;
    };

    let dep_names: Vec<String> = table.iter().map(|(k, _)| k.to_string()).collect();
    for dep_name in dep_names {
        let is_internal = table
            .get(&dep_name)
            .and_then(|d| d.as_table_like())
            .and_then(|t| t.get("package"))
            .and_then(|v| v.as_str())
            .is_some_and(|pkg| pkg.ends_with("-gpui-unofficial") || pkg == "gpui-unofficial");

        if is_internal {
            if let Some(dep) = table.get_mut(&dep_name) {
                if let Some(dep_table) = dep.as_table_like_mut() {
                    dep_table.insert("version", toml_edit::value(new_version));
                }
            }
        }
    }
}
