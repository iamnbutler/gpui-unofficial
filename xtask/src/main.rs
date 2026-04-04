mod bump;
mod publish;
mod transform;

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
        /// Dry run - don't actually publish
        #[arg(long)]
        dry_run: bool,

        /// Path to crates directory
        #[arg(long, default_value = "crates")]
        crates_dir: String,
    },

    /// Bump version of all crates (for patch releases)
    BumpVersion {
        /// New version (e.g., 0.230.2)
        version: String,

        /// Path to crates directory
        #[arg(long, default_value = "crates")]
        crates_dir: String,
    },

    /// Patch crate Cargo.tomls for publishing (strip git deps) without publishing
    PatchOnly {
        /// Path to crates directory
        #[arg(long, default_value = "crates")]
        crates_dir: String,
    },

    /// List crates in publish order
    ListCrates,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Transform {
            zed_tag,
            zed_path,
            output,
            local,
        } => transform::run(&zed_tag, zed_path.as_deref(), &output, local),

        Commands::Publish { dry_run, crates_dir } => publish::run(&crates_dir, dry_run),

        Commands::BumpVersion { version, crates_dir } => bump::run(&crates_dir, &version),

        Commands::PatchOnly { crates_dir } => publish::patch_only(&crates_dir),

        Commands::ListCrates => {
            for crate_name in transform::CRATE_PUBLISH_ORDER {
                println!("{crate_name}");
            }
            Ok(())
        }
    }
}
