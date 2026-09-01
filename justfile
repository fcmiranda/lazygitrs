default:
    just --list

ref-pull:
    bun scripts/fetch-references.ts pull

ref-clone:
    bun scripts/fetch-references.ts clone

preview:
    ./target/debug/lazygitrs

# as
rpreview:
    ./target/release/lazygitrs

# Release: bump versions, create a release commit, and push a git tag.
# Must be run from an up-to-date main — the script will refuse other branches.
tag: tag_and_release
tag_and_release:
    sh tag_and_release.sh

sync_readme:
    cp README.md npm/README.md

gen-benchmarks:
    bun scripts/gen-benchmarks.ts
    just sync_readme

install:
    cargo build --release
    mkdir -p "$HOME/.local/bin"
    install -m 755 target/release/lazygitrs "$HOME/.local/bin/lazygitrs"

# Build static x86_64 binary for Linux (musl)
build-x86:
    cargo zigbuild --release --target x86_64-unknown-linux-musl

# Build static ARM64 binary for Linux (musl)
build-arm:
    cargo zigbuild --release --target aarch64-unknown-linux-musl

# Build macOS Apple Silicon binary (aarch64)
build-mac-arm:
    cargo zigbuild --release --target aarch64-apple-darwin

# Build macOS Intel binary (x86_64)
build-mac-x86:
    cargo zigbuild --release --target x86_64-apple-darwin

# Build Windows binary (x86_64)
build-win:
    cargo zigbuild --release --target x86_64-pc-windows-gnu

# Package release archives for all architectures into dist/
dist version:
    mkdir -p dist
    @echo "Building Linux x86_64 (musl)..."
    cargo zigbuild --release --target x86_64-unknown-linux-musl
    tar -czf dist/lazygitrs-{{version}}-x86_64-unknown-linux-musl.tar.gz -C target/x86_64-unknown-linux-musl/release lazygitrs
    @echo "Building Linux ARM64 (musl)..."
    cargo zigbuild --release --target aarch64-unknown-linux-musl
    tar -czf dist/lazygitrs-{{version}}-aarch64-unknown-linux-musl.tar.gz -C target/aarch64-unknown-linux-musl/release lazygitrs
    @echo "Building macOS Apple Silicon..."
    cargo zigbuild --release --target aarch64-apple-darwin
    tar -czf dist/lazygitrs-{{version}}-aarch64-apple-darwin.tar.gz -C target/aarch64-apple-darwin/release lazygitrs
    @echo "Building Windows x86_64..."
    cargo zigbuild --release --target x86_64-pc-windows-gnu
    zip -q -j dist/lazygitrs-{{version}}-x86_64-pc-windows-gnu.zip target/x86_64-pc-windows-gnu/release/lazygitrs.exe
    @echo "Generating SHA256 checksums..."
    cd dist && sha256sum lazygitrs-{{version}}-* > SHA256SUMS.txt
    @echo "Artifacts generated in dist/:"
    @ls -lh dist/

