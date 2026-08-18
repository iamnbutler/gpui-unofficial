mod bump;
mod publish;
mod transform;
mod verify;
mod version;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::version::VersionIndex;

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
        /// Release version to stamp on the crates (e.g. 1.16.0-0). Defaults to
        /// revision 0 of the zed tag. See `resolve-version`.
        #[arg(long)]
        release_version: Option<String>,
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

    /// Print the version to publish for a zed tag, e.g. `1.16.0-0`.
    ///
    /// Every release carries a revision counter in the pre-release field so we
    /// can ship a fix without waiting for — or colliding with — zed's next tag.
    /// The counter is read back off crates.io, so this is the single source of
    /// truth for CI:
    ///
    ///   VERSION=$(cargo xtask resolve-version --zed-tag v1.16.0)
    ResolveVersion {
        /// Zed git tag to release (e.g. v1.16.0)
        #[arg(long)]
        zed_tag: String,
        /// Allocate a new revision above everything published — a hotfix of an
        /// already-released zed tag. Without this the highest published
        /// revision is re-used, so an interrupted release resumes in place.
        #[arg(long)]
        bump: bool,
        /// Use this exact revision instead of consulting crates.io
        #[arg(long, conflicts_with = "bump")]
        revision: Option<u32>,
        /// Crate whose published versions define the revision counter
        #[arg(long, default_value = "gpui-unofficial")]
        index_crate: String,
    },

    /// List crates in publish order
    ListCrates,

    /// Verify that a release is fully complete: release branch + GitHub release
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
        Commands::Transform { zed_tag, zed_path, output, local, release_version } =>
            transform::run(&zed_tag, zed_path.as_deref(), &output, local, release_version.as_deref()),

        Commands::Publish { dry_run, crates_dir } =>
            publish::run(&crates_dir, dry_run),

        Commands::BumpVersion { version, crates_dir } =>
            bump::run(&crates_dir, &version),

        Commands::PatchOnly { crates_dir } =>
            publish::patch_only(&crates_dir),

        Commands::ResolveVersion { zed_tag, bump, revision, index_crate } => {
            let mode = match (revision, bump) {
                (Some(revision), _) => version::RevisionMode::Exact(revision),
                (None, true) => version::RevisionMode::Bump,
                (None, false) => version::RevisionMode::Reuse,
            };
            let published = match mode {
                // An explicit revision needs no lookup, which also makes the
                // command usable offline.
                version::RevisionMode::Exact(_) => Vec::new(),
                _ => version::SparseIndex.published_versions(&index_crate)?,
            };
            let resolved = version::resolve(&zed_tag, &published, mode);
            eprintln!(
                "zed {zed_tag} -> {resolved} (revisions published: {:?})",
                version::revisions_for(&published, &resolved.upstream)
            );
            if resolved.revision == 0 && published.contains(&resolved.upstream) {
                // `x.y.z-0` sorts *below* a bare `x.y.z`, so cargo would never
                // offer it to anyone already on that release.
                eprintln!(
                    "warning: {} was published before the revision scheme; {resolved} would sort \
                     below it and reach nobody. Fixes to pre-scheme releases have to wait for \
                     zed's next tag — see docs/versioning.md.",
                    resolved.upstream
                );
            }
            // stdout carries only the version, for `$(...)` in CI.
            println!("{resolved}");
            Ok(())
        }

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
