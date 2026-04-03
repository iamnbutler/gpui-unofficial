# gpui-unofficial

Standalone releases of [Zed's gpui framework](https://github.com/zed-industries/zed/tree/main/crates/gpui) on crates.io, automatically kept in sync with Zed releases.

## Usage

```toml
[dependencies]
gpui-unofficial = "0.230"
```

Versions track Zed releases: Zed `v0.230.1` becomes `gpui-unofficial` version `0.230.1`.

## Crates

| Crate | Description |
|-------|-------------|
| `gpui-unofficial` | Main framework |
| `gpui-macros-unofficial` | Derive macros |
| `gpui-platform-unofficial` | Platform abstraction |
| `gpui-macos-unofficial` | macOS backend |
| `gpui-linux-unofficial` | Linux backend |
| `gpui-windows-unofficial` | Windows backend |
| `gpui-web-unofficial` | Web/WASM backend |

Plus supporting crates: `collections-unofficial`, `scheduler-unofficial`, `refineable-unofficial`, etc.

## How It Works

1. GitHub Actions checks for new Zed releases every 6 hours
2. Transforms the crates (renaming packages, updating dependencies)
3. Opens a PR, and on merge publishes to crates.io

## License

All gpui code is from [Zed](https://github.com/zed-industries/zed) and licensed under Apache-2.0.

This is an unofficial project not affiliated with Zed Industries. For official gpui, see [gpui.rs](https://gpui.rs).

## Development

```bash
# Transform a zed release
cargo xtask transform --zed-tag v0.230.1

# Use a local zed checkout
cargo xtask transform --zed-tag v0.230.1 --zed-path ../zed

# Path dependencies for local testing
cargo xtask transform --zed-tag v0.230.1 --zed-path ../zed --local

# Build
cd crates/gpui-unofficial && cargo build

# Publish dry run
cargo xtask publish --dry-run
```
