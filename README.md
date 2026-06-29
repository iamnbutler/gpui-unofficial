# gpui-unofficial

[![gpui-unofficial](https://img.shields.io/crates/v/gpui-unofficial.svg?label=gpui-unofficial)](https://crates.io/crates/gpui-unofficial)
[![gpui-platform-gpui-unofficial](https://img.shields.io/crates/v/gpui-platform-gpui-unofficial.svg?label=gpui-platform-gpui-unofficial)](https://crates.io/crates/gpui-platform-gpui-unofficial)

[![gpui-wgpu-gpui-unofficial](https://img.shields.io/crates/v/gpui-wgpu-gpui-unofficial.svg?label=gpui-wgpu-gpui-unofficial)](https://crates.io/crates/gpui-wgpu-gpui-unofficial)
[![gpui-macos-gpui-unofficial](https://img.shields.io/crates/v/gpui-macos-gpui-unofficial.svg?label=gpui-macos-gpui-unofficial)](https://crates.io/crates/gpui-macos-gpui-unofficial)
[![gpui-linux-gpui-unofficial](https://img.shields.io/crates/v/gpui-linux-gpui-unofficial.svg?label=gpui-linux-gpui-unofficial)](https://crates.io/crates/gpui-linux-gpui-unofficial)
[![gpui-windows-gpui-unofficial](https://img.shields.io/crates/v/gpui-windows-gpui-unofficial.svg?label=gpui-windows-gpui-unofficial)](https://crates.io/crates/gpui-windows-gpui-unofficial)
[![gpui-web-gpui-unofficial](https://img.shields.io/crates/v/gpui-web-gpui-unofficial.svg?label=gpui-web-gpui-unofficial)](https://crates.io/crates/gpui-web-gpui-unofficial)

[![gpui-macros-gpui-unofficial](https://img.shields.io/crates/v/gpui-macros-gpui-unofficial.svg?label=gpui-macros-gpui-unofficial)](https://crates.io/crates/gpui-macros-gpui-unofficial)
[![media-gpui-unofficial](https://img.shields.io/crates/v/media-gpui-unofficial.svg?label=media-gpui-unofficial)](https://crates.io/crates/media-gpui-unofficial)
[![reqwest-client-gpui-unofficial](https://img.shields.io/crates/v/reqwest-client-gpui-unofficial.svg?label=reqwest-client-gpui-unofficial)](https://crates.io/crates/reqwest-client-gpui-unofficial)
[![http-client-tls-gpui-unofficial](https://img.shields.io/crates/v/http-client-tls-gpui-unofficial.svg?label=http-client-tls-gpui-unofficial)](https://crates.io/crates/http-client-tls-gpui-unofficial)
[![http-client-gpui-unofficial](https://img.shields.io/crates/v/http-client-gpui-unofficial.svg?label=http-client-gpui-unofficial)](https://crates.io/crates/http-client-gpui-unofficial)
[![sum-tree-gpui-unofficial](https://img.shields.io/crates/v/sum-tree-gpui-unofficial.svg?label=sum-tree-gpui-unofficial)](https://crates.io/crates/sum-tree-gpui-unofficial)
[![scheduler-gpui-unofficial](https://img.shields.io/crates/v/scheduler-gpui-unofficial.svg?label=scheduler-gpui-unofficial)](https://crates.io/crates/scheduler-gpui-unofficial)

[![ztracing-gpui-unofficial](https://img.shields.io/crates/v/ztracing-gpui-unofficial.svg?label=ztracing-gpui-unofficial)](https://crates.io/crates/ztracing-gpui-unofficial)
[![ztracing-macro-gpui-unofficial](https://img.shields.io/crates/v/ztracing-macro-gpui-unofficial.svg?label=ztracing-macro-gpui-unofficial)](https://crates.io/crates/ztracing-macro-gpui-unofficial)
[![zlog-gpui-unofficial](https://img.shields.io/crates/v/zlog-gpui-unofficial.svg?label=zlog-gpui-unofficial)](https://crates.io/crates/zlog-gpui-unofficial)
[![util-gpui-unofficial](https://img.shields.io/crates/v/util-gpui-unofficial.svg?label=util-gpui-unofficial)](https://crates.io/crates/util-gpui-unofficial)
[![util-macros-gpui-unofficial](https://img.shields.io/crates/v/util-macros-gpui-unofficial.svg?label=util-macros-gpui-unofficial)](https://crates.io/crates/util-macros-gpui-unofficial)
[![perf-gpui-unofficial](https://img.shields.io/crates/v/perf-gpui-unofficial.svg?label=tooling-perf-gpui-unofficial)](https://crates.io/crates/perf-gpui-unofficial)
[![refineable-gpui-unofficial](https://img.shields.io/crates/v/refineable-gpui-unofficial.svg?label=refineable-gpui-unofficial)](https://crates.io/crates/refineable-gpui-unofficial)
[![derive-refineable-gpui-unofficial](https://img.shields.io/crates/v/derive-refineable-gpui-unofficial.svg?label=derive-refineable-gpui-unofficial)](https://crates.io/crates/derive-refineable-gpui-unofficial)
[![collections-gpui-unofficial](https://img.shields.io/crates/v/collections-gpui-unofficial.svg?label=collections-gpui-unofficial)](https://crates.io/crates/collections-gpui-unofficial)
[![gpui-shared-string-gpui-unofficial](https://img.shields.io/crates/v/gpui-shared-string-gpui-unofficial.svg?label=gpui-shared-string-gpui-unofficial)](https://crates.io/crates/gpui-shared-string-gpui-unofficial)
[![gpui-util-gpui-unofficial](https://img.shields.io/crates/v/gpui-util-gpui-unofficial.svg?label=gpui-util-gpui-unofficial)](https://crates.io/crates/gpui-util-gpui-unofficial)

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
