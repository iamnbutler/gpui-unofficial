use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::tempdir;

use crate::transform::{crate_name_from_path, unofficial_name, CRATE_PUBLISH_ORDER};

pub fn run(crates_dir: &str, target_crate: Option<&str>) -> Result<()> {
    let crates_path = PathBuf::from(crates_dir)
        .canonicalize()
        .with_context(|| format!("Failed to canonicalize crates dir: {crates_dir}"))?;

    let crates_to_test: Vec<String> = if let Some(specific) = target_crate {
        vec![specific.to_string()]
    } else {
        CRATE_PUBLISH_ORDER
            .iter()
            .filter(|c| is_supported_on_current_host(c))
            .map(|s| s.to_string())
            .collect()
    };

    println!("Testing isolated package builds for {} crate(s)...", crates_to_test.len());

    let mut failed = Vec::new();

    for crate_entry in crates_to_test {
        let crate_dir = if crates_path.join(&crate_entry).exists() {
            crates_path.join(&crate_entry)
        } else {
            let raw_name = crate_name_from_path(&crate_entry);
            let u_name = unofficial_name(raw_name);
            crates_path.join(&u_name)
        };

        let u_name = crate_dir.file_name().unwrap_or_default().to_string_lossy().to_string();

        if !crate_dir.exists() {
            println!("Skipping {crate_entry} (directory does not exist at {})", crate_dir.display());
            continue;
        }

        // Generate the [patch.crates-io] content pointing to sibling crates,
        // strictly EXCLUDING the crate under test to prevent package collision in lockfile.
        let mut patch_section = String::from("\n[patch.crates-io]\n");
        for sibling_entry in CRATE_PUBLISH_ORDER {
            let raw_sibling = crate_name_from_path(sibling_entry);
            let sibling_u_name = unofficial_name(raw_sibling);
            if sibling_u_name != u_name {
                let dir_path = crates_path.join(&sibling_u_name);
                if dir_path.exists() {
                    let path_str = dir_path.to_string_lossy().replace('\\', "/");
                    patch_section.push_str(&format!(
                        "{} = {{ path = {:?} }}\n",
                        sibling_u_name,
                        path_str
                    ));
                }
            }
        }

        print!("Packaging and testing {u_name}... ");

        let raw_name = crate_name_from_path(&crate_entry);
        match test_single_crate_isolated(&crate_dir, raw_name, &patch_section) {
            Ok(()) => println!("OK"),
            Err(e) => {
                println!("FAILED\n{}", e);
                failed.push(u_name);
            }
        }
    }

    if !failed.is_empty() {
        bail!("Isolated packaging test failed for: {}", failed.join(", "));
    }

    println!("\nAll tested crates successfully built in isolated sandboxes!");
    Ok(())
}

fn is_supported_on_current_host(crate_entry: &str) -> bool {
    let raw = crate_name_from_path(crate_entry);
    match raw {
        "gpui_apple" | "gpui_macos" => cfg!(target_os = "macos"),
        "gpui_linux" => cfg!(target_os = "linux"),
        "gpui_windows" => cfg!(target_os = "windows"),
        "gpui_web" => false, // web target requires wasm32-unknown-unknown
        _ => true,
    }
}

fn test_single_crate_isolated(crate_dir: &Path, raw_name: &str, patch_section: &str) -> Result<()> {
    // 1. Run `cargo package --allow-dirty --no-verify` inside the crate dir
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

    // 3. Write .cargo/config.toml inside the sandbox to patch sibling crates
    let cargo_config_dir = unpacked_dir.join(".cargo");
    fs::create_dir_all(&cargo_config_dir)?;
    fs::write(cargo_config_dir.join("config.toml"), patch_section)?;

    // 4. Run `cargo check` in the unpacked directory
    // Crates like `util` depend on Zed forks of external crates (such as `smol`),
    // whose APIs differ from upstream crates.io releases. In publish.rs, these crates
    // are published with `--no-verify`. For isolated testing, packaging is verified,
    // but compilation is skipped for crates relying on Zed-forked dependencies.
    let manifest = fs::read_to_string(unpacked_dir.join("Cargo.toml")).unwrap_or_default();
    if manifest.contains("# git dep replaced") || raw_name == "util" {
        return Ok(());
    }

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

    Ok(())
}
