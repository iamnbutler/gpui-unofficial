# gpui-unofficial

Automated, unofficial releases of [Zed's gpui framework](https://github.com/zed-industries/zed/tree/main/crates/gpui) as GitHub releases.

## Why?

Zed published gpui to crates.io once in 2024 and hasn't updated it since. The framework continues evolving but isn't available as a versioned crate, forcing projects to use git dependencies.

This project automatically transforms and publishes gpui (and its dependencies) whenever Zed cuts a new release.

## Usage

```toml
[dependencies]
# Use a specific release
gpui-unofficial = { git = "https://github.com/gpui-unofficial/gpui-unofficial", tag = "v0.230.1" }

# Or use latest main
gpui-unofficial = { git = "https://github.com/gpui-unofficial/gpui-unofficial" }
```

## Crates Included

- `gpui-unofficial` - Main framework
- `gpui-macros-unofficial` - Derive macros
- `gpui-platform-unofficial` - Platform abstraction
- `gpui-macos-unofficial` - macOS backend
- `gpui-linux-unofficial` - Linux backend
- `gpui-windows-unofficial` - Windows backend
- `gpui-web-unofficial` - Web/WASM backend
- And supporting crates: `collections-unofficial`, `scheduler-unofficial`, etc.

## Versioning

Versions track Zed releases: Zed `v0.230.1` becomes `gpui-unofficial` `0.230.1`.

## How It Works

1. GitHub Actions checks for new Zed releases every 6 hours
2. When a new release is found, it transforms the crates (renaming, updating dependencies)
3. Creates a PR with the updated crates
4. On merge, creates a GitHub release with the transformed crates

## License

All code is from Zed and licensed under Apache-2.0.

## Disclaimer

This is an unofficial project not affiliated with Zed Industries. For official gpui support, see [gpui.rs](https://gpui.rs).

## Development

```bash
# Transform a zed release
cargo xtask transform --zed-tag v0.230.1

# Or use a local zed checkout
cargo xtask transform --zed-tag v0.230.1 --zed-path ../zed

# Use path dependencies for local testing
cargo xtask transform --zed-tag v0.230.1 --zed-path ../zed --local

# Build transformed crates
cd crates/gpui-unofficial && cargo build
```
