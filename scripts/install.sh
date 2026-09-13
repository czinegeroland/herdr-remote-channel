#!/usr/bin/env sh
# Installs a prebuilt `hrc` executable (PRD requirements HRC-TECH-011 and
# HRC-TECH-012, section 13.2).
#
# The requirement this script exists to satisfy is a negative one: installing
# must need no Rust toolchain and no .NET, Node.js, or Python runtime on the
# end-user machine. So it is POSIX shell using only tools a base system
# already has — `curl` or `wget`, `tar`, and one of `sha256sum` or `shasum` —
# and it downloads a prebuilt binary rather than compiling anything.
#
# It refuses to install an artifact whose checksum does not match. That is
# the point of publishing checksums; a verification that can be skipped by
# accident is not one.
#
#   install.sh [--version <tag>] [--prefix <dir>] [--base-url <url>]
#
# `--base-url` exists so the release fixture can point at a local directory
# and exercise this exact script rather than a copy of its logic.

set -eu

REPO="czinegeroland/herdr-remote-channel"
VERSION=""
PREFIX="${HRC_INSTALL_PREFIX:-$HOME/.local/bin}"
BASE_URL=""

die() {
    echo "install.sh: $*" >&2
    exit 1
}

while [ $# -gt 0 ]; do
    case "$1" in
        --version) VERSION="${2:-}"; shift 2 ;;
        --prefix)  PREFIX="${2:-}"; shift 2 ;;
        --base-url) BASE_URL="${2:-}"; shift 2 ;;
        -h|--help)
            sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
            exit 0 ;;
        *) die "unknown option $1" ;;
    esac
done

[ -n "$VERSION" ] || die "a --version is required, for example --version v0.1.0"

# Which artifact this machine needs. An unrecognized platform is an error
# rather than a guess: installing the wrong binary fails later and less
# clearly than refusing now.
os="$(uname -s)"
arch="$(uname -m)"
case "${os}:${arch}" in
    Linux:x86_64)              target="x86_64-unknown-linux-gnu" ;;
    Darwin:x86_64)             target="x86_64-apple-darwin" ;;
    Darwin:arm64|Darwin:aarch64) target="aarch64-apple-darwin" ;;
    *) die "no prebuilt binary for ${os} ${arch}; see the release page" ;;
esac

archive="hrc-${VERSION}-${target}.tar.gz"
if [ -z "$BASE_URL" ]; then
    BASE_URL="https://github.com/${REPO}/releases/download/${VERSION}"
fi

work="$(mktemp -d)"
# shellcheck disable=SC2064
trap "rm -rf '${work}'" EXIT INT TERM

fetch() {
    # $1 source, $2 destination
    case "$BASE_URL" in
        file://*|/*)
            src="${1#file://}"
            [ -f "$src" ] || die "no such file: ${src}"
            cp "$src" "$2"
            ;;
        *)
            if command -v curl > /dev/null 2>&1; then
                curl --fail --silent --show-error --location "$1" --output "$2"
            elif command -v wget > /dev/null 2>&1; then
                wget --quiet "$1" -O "$2"
            else
                die "neither curl nor wget is available"
            fi
            ;;
    esac
}

echo "Downloading ${archive}"
fetch "${BASE_URL}/${archive}" "${work}/${archive}"
fetch "${BASE_URL}/${archive}.sha256" "${work}/${archive}.sha256"

# The published checksum file is `<digest>  <name>`, and the name in it may
# carry a path from wherever it was generated. Only the digest is compared,
# so a path difference cannot be mistaken for a mismatch — and a mismatch
# cannot be mistaken for a path difference.
expected="$(cut -d' ' -f1 < "${work}/${archive}.sha256")"
[ -n "$expected" ] || die "the published checksum file is empty"

if command -v sha256sum > /dev/null 2>&1; then
    actual="$(sha256sum "${work}/${archive}" | cut -d' ' -f1)"
elif command -v shasum > /dev/null 2>&1; then
    actual="$(shasum -a 256 "${work}/${archive}" | cut -d' ' -f1)"
else
    die "neither sha256sum nor shasum is available; cannot verify the download"
fi

if [ "$expected" != "$actual" ]; then
    die "checksum mismatch for ${archive}
  published: ${expected}
  actual:    ${actual}
Refusing to install. The artifact does not match what the release published."
fi
echo "Checksum verified: ${actual}"

tar -xzf "${work}/${archive}" -C "${work}"
binary="${work}/hrc-${VERSION}-${target}/hrc"
[ -f "$binary" ] || die "the archive did not contain an hrc executable"

mkdir -p "$PREFIX"
# Installed by moving a fully written file into place, so an interrupted
# install cannot leave a half-copied executable on the path.
staged="${PREFIX}/.hrc.$$"
cp "$binary" "$staged"
chmod +x "$staged"
mv -f "$staged" "${PREFIX}/hrc"

echo "Installed ${PREFIX}/hrc"
"${PREFIX}/hrc" --version
