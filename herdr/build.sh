#!/usr/bin/env sh
# Puts an `hrc` executable where the Herdr plugin manifest expects one.
#
# Herdr runs this once, at `herdr plugin install` time, with the plugin
# directory as the working directory and before it registers the plugin. A
# failure here halts the installation, which is what we want: a plugin
# registered without its binary fails later and less clearly.
#
# Two outcomes, in order of preference:
#
#   1. A published release artifact, downloaded and checksum-verified by
#      `scripts/install.sh`. No Rust toolchain required.
#   2. `cargo build --release`, for a platform no release covers, for a
#      checkout newer than the last tag, and for the period before the first
#      tag exists at all.
#
# Either way the binary ends up in two places, because two different things
# look for it:
#
#   * `bin/hrc` under the plugin root, which is what the manifest invokes.
#     Resolving it relative to the plugin root rather than through PATH is
#     what makes the plugin work on a machine where the install prefix is not
#     on the PATH Herdr inherited.
#   * `$HRC_INSTALL_PREFIX` (default `~/.local/bin`), so that `hrc send`,
#     `hrc doctor`, and the agent skill's `hrc --help` discovery work in a
#     terminal. This copy is a convenience and its failure is not fatal.

set -eu

plugin_root="$(cd "$(dirname "$0")/.." && pwd)"
prefix="${HRC_INSTALL_PREFIX:-$HOME/.local/bin}"
cd "$plugin_root"

say() { echo "herdr/build.sh: $*"; }

install_to_plugin_root() {
    mkdir -p bin
    # Staged then moved, so a half-copied file is never left where the
    # manifest points. Herdr may start the plugin as soon as this returns.
    cp "$1" bin/.hrc.$$
    chmod +x bin/.hrc.$$
    mv -f bin/.hrc.$$ bin/hrc
    say "installed $plugin_root/bin/hrc"
}

# The tag to try, if the caller did not name one. `git describe` is the only
# thing here that knows about releases; a checkout with no tags simply has no
# answer and falls through to building.
version="${HRC_VERSION:-}"
if [ -z "$version" ] && command -v git >/dev/null 2>&1; then
    version="$(git describe --tags --abbrev=0 2>/dev/null || true)"
fi

if [ -n "$version" ]; then
    say "trying published release $version"
    # Installed to a scratch prefix first: `scripts/install.sh` verifies a
    # checksum and refuses a mismatch, and letting it write straight into
    # `bin/` would mean a failed verification had already replaced a working
    # binary.
    staging="$(mktemp -d)"
    if HRC_INSTALL_PREFIX="$staging" sh scripts/install.sh --version "$version" --prefix "$staging"; then
        install_to_plugin_root "$staging/hrc"
        mkdir -p "$prefix" && cp "$staging/hrc" "$prefix/hrc" 2>/dev/null \
            && say "installed $prefix/hrc" \
            || say "could not write $prefix/hrc; the plugin still works, but the CLI is not on PATH"
        rm -rf "$staging"
        exit 0
    fi
    rm -rf "$staging"
    say "no usable release artifact for this platform; building from source"
fi

command -v cargo >/dev/null 2>&1 || {
    echo "herdr/build.sh: no published release covers this platform and cargo is not installed." >&2
    echo "Install a Rust toolchain from https://rustup.rs and retry, or install a" >&2
    echo "prebuilt hrc yourself and copy it to $plugin_root/bin/hrc." >&2
    exit 1
}

say "building from source (this takes a few minutes the first time)"
cargo build --release --locked --bin hrc
install_to_plugin_root target/release/hrc

mkdir -p "$prefix" && cp target/release/hrc "$prefix/hrc" 2>/dev/null \
    && say "installed $prefix/hrc" \
    || say "could not write $prefix/hrc; the plugin still works, but the CLI is not on PATH"

say "done"
