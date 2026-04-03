use anyhow::{bail, Result};
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::transform::CRATE_PUBLISH_ORDER;

/// Delay between publishing crates to allow crates.io to propagate
const PUBLISH_DELAY: Duration = Duration::from_secs(30);

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

    for (i, crate_name) in CRATE_PUBLISH_ORDER.iter().enumerate() {
        let unofficial_name = crate_name.replace('_', "-") + "-unofficial";
        let crate_path = crates_path.join(&unofficial_name);

        if !crate_path.exists() {
            println!("Skipping {unofficial_name} (not found)");
            continue;
        }

        println!(
            "[{}/{}] Publishing {unofficial_name}...",
            i + 1,
            CRATE_PUBLISH_ORDER.len()
        );

        let mut cmd = Command::new("cargo");
        cmd.arg("publish");

        if dry_run {
            cmd.arg("--dry-run");
        }

        cmd.current_dir(&crate_path);

        let status = cmd.status()?;

        if !status.success() {
            bail!("Failed to publish {unofficial_name}");
        }

        // Wait for crates.io propagation (except for dry run or last crate)
        if !dry_run && i < CRATE_PUBLISH_ORDER.len() - 1 {
            println!("Waiting {PUBLISH_DELAY:?} for crates.io propagation...");
            thread::sleep(PUBLISH_DELAY);
        }
    }

    println!("\nPublish complete!");

    Ok(())
}
