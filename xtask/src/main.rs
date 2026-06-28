mod bump;
mod publish;
mod transform;
mod verify;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask")]
#[command(about = "Build automation for gpui-unofficial")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Transform zed's gpui crates for standalone publishing
    Transform {
        /// Zed git tag to transform (e.g., v0.185.0)
        #[arg(long)]
        zed_tag: String,
        /// Path to local zed repo (optional, will clone if not provided)
        #[arg(long)]
        zed_path: Option<String>,
        /// Output directory for transformed crates (default: ./crates)
        #[arg(long, default_value = "crates")]
        output: String,
        /// Use path dependencies for local testing (instead of version deps)
        #[arg(long)]
        local: bool,
    },

    /// Publish crates to crates.io in dependency order
    Publish {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value = "crates")]
        crates_dir: String,
    },

    /// Bump version of all crates (for patch releases)
    BumpVersion {
        version: String,
        #[arg(long, default_value = "crates")]
        crates_dir: String,
    },

    /// Patch crate Cargo.tomls for publishing (strip git deps) without publishing
    PatchOnly {
        #[arg(long, default_value = "crates")]
        crates_dir: String,
    },

    /// List crates in publish order
    ListCrates,

    /// Verify that a release is fully complete: release branch + GitHub release  ← add this variant
    /// + all crates published to crates.io.
    ///
    /// Exits 0 if complete, 1 if anything is missing. Use as a CI skip-guard:
    ///
    ///   if cargo xtask verify --tag v1.8.2; then echo "already done"; fi
    Verify {
        /// Version tag to verify (e.g. v1.8.2)
        #[arg(long)]
        tag: String,
        /// GitHub repo in owner/name format
        #[arg(long, default_value = "iamnbutler/gpui-unofficial")]
        repo: String,
        /// Print per-crate publish status
        #[arg(long)]
        verbose: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Transform { zed_tag, zed_path, output, local } =>
            transform::run(&zed_tag, zed_path.as_deref(), &output, local),

        Commands::Publish { dry_run, crates_dir } =>
            publish::run(&crates_dir, dry_run),

        Commands::BumpVersion { version, crates_dir } =>
            bump::run(&crates_dir, &version),

        Commands::PatchOnly { crates_dir } =>
            publish::patch_only(&crates_dir),

        Commands::ListCrates => {
            for crate_name in transform::CRATE_PUBLISH_ORDER {
                println!("{crate_name}");
            }
            Ok(())
        }

        Commands::Verify { tag, repo, verbose } => {
            let complete = verify::run(&tag, &repo, None, verbose)?;
            if !complete {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}
