use anyhow::{bail, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;
use toml_edit::DocumentMut;
use walkdir::WalkDir;

use crate::transform::{crate_name_from_path, unofficial_name, CRATE_PUBLISH_ORDER};

pub fn run(crates_dir: &str, target_crate: Option<&str>) -> Result<()> {
    let crates_path = PathBuf::from(crates_dir)
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize crates dir: {crates_dir}"))?;

    // Every crate transform.rs actually produced on disk, in topological
    // (dependency-first) order. `cargo package`'s lockfile-resolution phase
    // considers every `[target.'cfg(...)']` dependency across all platforms
    // regardless of host, so every sibling that exists on disk must be
    // packageable (and vendored, see below) even if this run isn't asked to
    // fully test it — otherwise later crates in the order fail to resolve it.
    let all_crates: Vec<(String, PathBuf, &'static str)> = CRATE_PUBLISH_ORDER
        .iter()
        .filter_map(|&entry| {
            let raw_name = crate_name_from_path(entry);
            let u_name = unofficial_name(raw_name);
            let dir = crates_path.join(&u_name);
            dir.exists().then_some((u_name, dir, raw_name))
        })
        .collect();

    let test_raw_names: Vec<&str> = if let Some(specific) = target_crate {
        vec![specific]
    } else {
        CRATE_PUBLISH_ORDER
            .iter()
            .map(|entry| crate_name_from_path(entry))
            .filter(|raw| is_supported_on_current_host(raw))
            .collect()
    };

    println!("Testing isolated package builds for {} crate(s)...", test_raw_names.len());

    // `cargo package`'s dependency resolution ignores `[patch]`/`[replace]`
    // sections entirely, so it can't be redirected to sibling crates that
    // way (the previous approach). It DOES honor a `[source] replace-with`
    // directory-source override. A single `cargo vendor` run from a
    // synthetic aggregator crate that path-depends on every crate on disk
    // captures the full union of genuine external (crates.io) dependencies
    // in one shot (vendoring skips path dependencies, so internal siblings
    // are left out). Each internal sibling is then added to this same
    // directory incrementally below, right after it is successfully
    // packaged, so later crates in `CRATE_PUBLISH_ORDER` can resolve it
    // without requiring a prior real publish.
    let vendor_dir = tempdir().context("Failed to create vendor directory")?;
    build_external_vendor_dir(&all_crates, vendor_dir.path())?;
    write_source_replace_config(&crates_path.join(".cargo").join("config.toml"), vendor_dir.path())?;

    let mut failed = Vec::new();

    for (u_name, crate_dir, raw_name) in &all_crates {
        let is_under_test = test_raw_names.contains(raw_name);
        if is_under_test {
            print!("Packaging and testing {u_name}... ");
        }

        match package_and_vendor(crate_dir, u_name, raw_name, is_under_test, vendor_dir.path()) {
            Ok(()) => {
                if is_under_test {
                    println!("OK");
                }
            }
            Err(e) => {
                if is_under_test {
                    println!("FAILED\n{}", e);
                } else {
                    // Not under test on this host/run, but still required to
                    // seed the vendor directory for downstream siblings.
                    println!("Seeding {u_name} into the vendor directory FAILED\n{}", e);
                }
                failed.push(u_name.clone());
            }
        }
    }

    if !failed.is_empty() {
        bail!("Isolated packaging test failed for: {}", failed.join(", "));
    }

    println!("\nAll tested crates successfully built in isolated sandboxes!");
    Ok(())
}

fn is_supported_on_current_host(raw_name: &str) -> bool {
    match raw_name {
        "gpui_apple" | "gpui_macos" => cfg!(target_os = "macos"),
        "gpui_linux" => cfg!(target_os = "linux"),
        "gpui_windows" => cfg!(target_os = "windows"),
        "gpui_web" => false, // web target requires wasm32-unknown-unknown
        _ => true,
    }
}

/// Build a directory source containing every genuine external (crates.io)
/// dependency reachable from any crate on disk, by vendoring from a
/// synthetic crate that path-depends on all of them at once. Path
/// dependencies resolve directly against the listed path (ordinary Cargo
/// resolution, unlike packaging resolution, honors local paths), so only
/// true external dependencies end up here.
fn build_external_vendor_dir(all_crates: &[(String, PathBuf, &str)], vendor_dir: &Path) -> Result<()> {
    let aggregator = tempdir().context("Failed to create aggregator directory")?;
    let aggregator_path = aggregator.path();
    fs::create_dir_all(aggregator_path.join("src"))?;
    fs::write(aggregator_path.join("src/lib.rs"), "")?;

    let mut manifest = String::from(
        "[package]\nname = \"vendor-aggregator\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[dependencies]\n",
    );
    let mut extra_dev_deps: BTreeMap<String, String> = BTreeMap::new();
    for (u_name, dir, raw_name) in all_crates {
        let path_str = dir.to_string_lossy().replace('\\', "/");
        // Match the extra features `package_and_vendor`'s `cargo check` step
        // enables, so their optional dependencies get vendored too.
        if cfg!(target_os = "linux") && (*raw_name == "gpui" || *raw_name == "gpui_linux") {
            manifest.push_str(&format!(
                "{u_name} = {{ path = {path_str:?}, features = [\"wayland\", \"x11\"] }}\n"
            ));
        } else {
            manifest.push_str(&format!("{u_name} = {{ path = {path_str:?} }}\n"));
        }

        collect_external_dev_dependencies(dir, &mut extra_dev_deps)?;
    }

    // `cargo package`'s lockfile-resolution phase for each crate (run per
    // crate below) also resolves that crate's own `[dev-dependencies]`, even
    // with `--no-verify`. But `cargo vendor` only pulls in dev-dependencies
    // of workspace *members* — a plain path dependency's dev-dependencies
    // (like these) are never reached, so add them here as ordinary
    // dependencies of the aggregator so they still land in the vendor dir.
    for (name, req) in &extra_dev_deps {
        manifest.push_str(&format!("{name} = {req}\n"));
    }

    manifest.push_str("\n[workspace]\n");
    fs::write(aggregator_path.join("Cargo.toml"), manifest)?;

    let output = Command::new("cargo")
        .args(["vendor", "--versioned-dirs"])
        .arg(vendor_dir)
        .current_dir(aggregator_path)
        .output()
        .context("Failed to run cargo vendor")?;

    if !output.status.success() {
        bail!(
            "cargo vendor failed while collecting external dependencies:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    Ok(())
}

/// Collect every non-path `[dev-dependencies]` entry declared by the crate at
/// `dir` (including target-gated tables like
/// `[target.'cfg(...)'.dev-dependencies]`), so `build_external_vendor_dir`
/// can vendor them too. Entries already present in `out` are left as-is.
fn collect_external_dev_dependencies(dir: &Path, out: &mut BTreeMap<String, String>) -> Result<()> {
    let manifest_path = dir.join("Cargo.toml");
    let Ok(content) = fs::read_to_string(&manifest_path) else {
        return Ok(());
    };
    let doc: DocumentMut = content
        .parse()
        .with_context(|| format!("Failed to parse {}", manifest_path.display()))?;

    let mut dev_dep_tables = Vec::new();
    if let Some(item) = doc.get("dev-dependencies") {
        dev_dep_tables.push(item);
    }
    if let Some(target) = doc.get("target").and_then(|t| t.as_table_like()) {
        for (_, cfg_item) in target.iter() {
            if let Some(dev_deps) = cfg_item.get("dev-dependencies") {
                dev_dep_tables.push(dev_deps);
            }
        }
    }

    for table_item in dev_dep_tables {
        let Some(table) = table_item.as_table_like() else {
            continue;
        };
        for (name, value) in table.iter() {
            let is_path_dep = value.as_table_like().is_some_and(|t| t.get("path").is_some());
            if is_path_dep {
                continue;
            }
            out.entry(name.to_string())
                .or_insert_with(|| value.to_string().trim().to_string());
        }
    }

    Ok(())
}

fn write_source_replace_config(config_path: &Path, vendor_dir: &Path) -> Result<()> {
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let vendor_str = vendor_dir.to_string_lossy().replace('\\', "/");
    fs::write(
        config_path,
        format!(
            "[source.crates-io]\nreplace-with = \"vendored-sources\"\n\n[source.vendored-sources]\ndirectory = \"{vendor_str}\"\n"
        ),
    )?;
    Ok(())
}

fn package_and_vendor(
    crate_dir: &Path,
    u_name: &str,
    raw_name: &str,
    run_check: bool,
    vendor_dir: &Path,
) -> Result<()> {
    // 1. Run `cargo package --allow-dirty --no-verify` inside the crate dir.
    // Cargo's config search walks up from `crate_dir` through its ancestors,
    // so this picks up the `[source] replace-with` config written at the
    // shared `crates_path` level without needing a copy per crate.
    let pkg_output = Command::new("cargo")
        .args(["package", "--allow-dirty", "--no-verify"])
        .current_dir(crate_dir)
        .output()
        .context("Failed to run cargo package")?;

    if !pkg_output.status.success() {
        let stderr = String::from_utf8_lossy(&pkg_output.stderr);
        bail!("cargo package failed:\n{stderr}");
    }

    // Locate the generated .crate file in target/package/
    let package_dir = crate_dir.join("target/package");
    let mut crate_file: Option<PathBuf> = None;
    if package_dir.exists() {
        for entry in fs::read_dir(&package_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "crate") {
                crate_file = Some(path);
                break;
            }
        }
    }

    let crate_file = match crate_file {
        Some(f) => f,
        None => bail!("Could not find .crate file in {}", package_dir.display()),
    };

    // 2. Create an isolated sandbox temporary directory
    let sandbox = tempdir().context("Failed to create temporary directory")?;
    let sandbox_path = sandbox.path();

    // Extract the .crate tarball into the sandbox
    let tar_status = Command::new("tar")
        .args(["-xzf", crate_file.to_str().unwrap(), "-C", sandbox_path.to_str().unwrap()])
        .status()
        .context("Failed to run tar to unpack .crate")?;

    if !tar_status.success() {
        bail!("Failed to unpack {}", crate_file.display());
    }

    // Find the unpacked crate folder inside the sandbox (e.g. `sandbox/<name>-<version>/`)
    let mut unpacked_dir: Option<PathBuf> = None;
    for entry in fs::read_dir(sandbox_path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            unpacked_dir = Some(entry.path());
            break;
        }
    }

    let unpacked_dir = match unpacked_dir {
        Some(d) => d,
        None => bail!("No extracted directory found in sandbox"),
    };

    if run_check {
        // 3. Write .cargo/config.toml inside the sandbox so it resolves
        // external and (already-vendored) internal sibling deps the same
        // way the packaging step did.
        write_source_replace_config(&unpacked_dir.join(".cargo").join("config.toml"), vendor_dir)?;

        // 4. Run `cargo check` in the unpacked directory.
        // Crates like `util` depend on Zed forks of external crates (such as `smol`),
        // whose APIs differ from upstream crates.io releases. In publish.rs, these crates
        // are published with `--no-verify`. For isolated testing, packaging is verified,
        // but compilation is skipped for crates relying on Zed-forked dependencies.
        let manifest = fs::read_to_string(unpacked_dir.join("Cargo.toml")).unwrap_or_default();
        if !(manifest.contains("# git dep replaced") || raw_name == "util") {
            let mut check_cmd = Command::new("cargo");
            check_cmd.arg("check");
            if cfg!(target_os = "linux") && (raw_name == "gpui" || raw_name == "gpui_linux") {
                check_cmd.args(["--features", "wayland,x11"]);
            }

            let check_output = check_cmd
                .current_dir(&unpacked_dir)
                .output()
                .context("Failed to execute cargo check in sandbox")?;

            if !check_output.status.success() {
                let stderr = String::from_utf8_lossy(&check_output.stderr);
                bail!("cargo check failed in isolated package sandbox:\n{stderr}");
            }
        }
    }

    // 5. Add this crate's packaged output to the shared vendor directory so
    // crates later in `CRATE_PUBLISH_ORDER` can resolve it as a dependency.
    add_to_vendor_dir(&unpacked_dir, u_name, vendor_dir)?;

    Ok(())
}

/// Copy a packaged crate's extracted contents into the vendor directory as a
/// `<name>-<version>/` directory source entry. `.cargo-checksum.json` with
/// empty checksums is accepted by Cargo's directory-source loader and skips
/// file-level verification, which is fine here since the contents are
/// produced by our own trusted `cargo package` step immediately prior.
fn add_to_vendor_dir(unpacked_dir: &Path, u_name: &str, vendor_dir: &Path) -> Result<()> {
    let manifest = fs::read_to_string(unpacked_dir.join("Cargo.toml"))
        .context("Failed to read packaged Cargo.toml")?;
    let doc: DocumentMut = manifest
        .parse()
        .context("Failed to parse packaged Cargo.toml")?;
    let version = doc["package"]["version"]
        .as_str()
        .context("Packaged Cargo.toml has no package.version")?
        .to_string();

    let dest = vendor_dir.join(format!("{u_name}-{version}"));
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_dir_recursive(unpacked_dir, &dest)?;
    fs::write(dest.join(".cargo-checksum.json"), r#"{"files":{},"package":""}"#)?;

    Ok(())
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest)?;

    for entry in WalkDir::new(src) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(src)?;
        if relative.as_os_str().is_empty() {
            continue;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_plain_and_targeted_external_dev_deps_but_skips_path_deps() {
        let dir = tempdir().unwrap();
        fs::write(
            dir.path().join("Cargo.toml"),
            r#"
[package]
name = "foo"
version = "1.0.0"

[dependencies]
bar = { path = "../bar", version = "1.0.0" }

[dev-dependencies]
pretty_assertions = "1.4.1"
sibling-gpui-unofficial = { path = "../sibling", version = "1.0.0" }

[target.'cfg(not(target_family = "wasm"))'.dev-dependencies]
proptest = { version = "1.5", features = ["std"] }
"#,
        )
        .unwrap();

        let mut out = BTreeMap::new();
        collect_external_dev_dependencies(dir.path(), &mut out).unwrap();

        assert_eq!(out.get("pretty_assertions").map(String::as_str), Some("\"1.4.1\""));
        assert!(out.get("proptest").unwrap().contains("1.5"));
        assert!(!out.contains_key("sibling-gpui-unofficial"));
    }
}
