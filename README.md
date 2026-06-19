# gpui-unofficial

Fully automated standalones release of [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) on each new Zed release tag.

## Usage

```toml
[dependencies]
gpui-unofficial = "1.7"   # pick the version matching the Zed release you want
```

Versions mirror Zed's release tags: Zed `v1.7.2` publishes as `gpui-unofficial`
version `1.7.2`. The platform backends are pulled in by `gpui-unofficial`, so
this single dependency is all you need to get started.

Because the version is taken verbatim from Zed's semver, there is currently no
way to publish a fix for an already-released version without a version suffix —
a known limitation.

## Crates

| Crate | Description |
|-------|-------------|
| `gpui-unofficial` | Main framework |
| `gpui-macros-gpui-unofficial` | Derive macros |
| `gpui-platform-gpui-unofficial` | Platform abstraction |
| `gpui-macos-gpui-unofficial` | macOS backend |
| `gpui-linux-gpui-unofficial` | Linux backend |
| `gpui-windows-gpui-unofficial` | Windows backend |
| `gpui-web-gpui-unofficial` | Web/WASM backend |

Plus supporting crates: `collections-gpui-unofficial`, `scheduler-gpui-unofficial`, `refineable-gpui-unofficial`, etc.

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
