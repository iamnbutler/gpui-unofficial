# gpui-unofficial

Automated, unofficial releases of [Zed's gpui framework](https://github.com/zed-industries/zed/tree/main/crates/gpui) to crates.io.

## Why?

Zed published gpui to crates.io once in 2024 and hasn't updated it since. The framework continues evolving but isn't available as a versioned crate, forcing projects to use git dependencies.

This project automatically transforms and publishes gpui (and its dependencies) whenever Zed cuts a new release.

## Usage

```toml
[dependencies]
gpui-unofficial = "0.185"  # Tracks zed release versions

# Or with platform selection:
gpui-platform-unofficial = { version = "0.185", features = ["macos"] }
```

## Crates Published

- `gpui-unofficial` - Main framework
- `gpui-macros-unofficial` - Derive macros
- `gpui-platform-unofficial` - Platform abstraction
- `gpui-macos-unofficial` - macOS backend
- `gpui-linux-unofficial` - Linux backend
- `gpui-windows-unofficial` - Windows backend
- `gpui-web-unofficial` - Web/WASM backend
- And supporting crates: `collections-unofficial`, `scheduler-unofficial`, etc.

## Versioning

Versions track Zed releases: Zed `v0.185.0` becomes `gpui-unofficial` `0.185.0`.

## License

All code is from Zed and licensed under Apache-2.0.

## Disclaimer

This is an unofficial project not affiliated with Zed Industries. For official gpui support, see [gpui.rs](https://gpui.rs).

## Development

```bash
# Transform latest zed release
cargo xtask transform --zed-tag v0.185.0

# Or use a local zed checkout
cargo xtask transform --zed-tag v0.185.0 --zed-path ../zed

# Build transformed crates
cargo build --manifest-path crates/gpui-unofficial/Cargo.toml

# Publish (dry run)
cargo xtask publish --dry-run

# Publish for real
cargo xtask publish
```
