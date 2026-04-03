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
        } => transform::run(&zed_tag, zed_path.as_deref(), &output),

        Commands::Publish { dry_run, crates_dir } => publish::run(&crates_dir, dry_run),

        Commands::ListCrates => {
            for crate_name in transform::CRATE_PUBLISH_ORDER {
                println!("{crate_name}");
            }
            Ok(())
        }
    }
}
