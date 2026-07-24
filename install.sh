#!/bin/sh

set -eu

repository_url="https://github.com/jomof/git-repo-worktree"
release_version="${GIT_REPO_WORKTREE_VERSION:-latest}"
if [ -n "${GIT_REPO_WORKTREE_INSTALL_DIR:-}" ]; then
    install_dir=$GIT_REPO_WORKTREE_INSTALL_DIR
elif [ -n "${HOME:-}" ]; then
    install_dir=$HOME/.local/bin
else
    install_dir=""
fi
create_git_alias=1
temporary_dir=""
install_temporary=""

say() {
    printf '%s\n' "$*"
}

warn() {
    printf 'warning: %s\n' "$*" >&2
}

fail() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

usage() {
    cat <<'EOF'
Install git-repo-worktree from a GitHub release.

Usage:
  install.sh [--version VERSION] [--install-dir DIR] [--no-git-alias]

Options:
  --version VERSION   Install a release such as v1.0.0 (default: latest).
  --install-dir DIR   Install the binary into DIR (default: ~/.local/bin).
  --no-git-alias      Do not configure a global Git alias when DIR is off PATH.
  -h, --help          Show this help.

Environment:
  GIT_REPO_WORKTREE_VERSION       Default value for --version.
  GIT_REPO_WORKTREE_INSTALL_DIR   Default value for --install-dir.
EOF
}

cleanup() {
    if [ -n "$install_temporary" ]; then
        rm -f "$install_temporary"
    fi
    if [ -n "$temporary_dir" ]; then
        rm -rf "$temporary_dir"
    fi
}

trap cleanup 0
trap 'exit 1' 1 2 15

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            shift
            [ "$#" -gt 0 ] || fail "--version requires a value"
            release_version=$1
            ;;
        --install-dir)
            shift
            [ "$#" -gt 0 ] || fail "--install-dir requires a value"
            install_dir=$1
            ;;
        --no-git-alias)
            create_git_alias=0
            ;;
        -h | --help)
            usage
            exit 0
            ;;
        *)
            fail "unknown option: $1"
            ;;
    esac
    shift
done

[ -n "$install_dir" ] ||
    fail "HOME is unset; provide an install directory with --install-dir"

for command_name in curl tar git awk sed tr mkdir cp chmod mv uname mktemp; do
    command -v "$command_name" >/dev/null 2>&1 ||
        fail "required command not found: $command_name"
done

case "$install_dir" in
    /*) ;;
    *) install_dir=$PWD/$install_dir ;;
esac
mkdir -p "$install_dir"
install_dir=$(CDPATH='' cd "$install_dir" && pwd -P)
destination="$install_dir/git-repo-worktree"

shell_quote() {
    printf "'"
    printf '%s' "$1" | sed "s/'/'\\\\''/g"
    printf "'"
}

quoted_destination=$(shell_quote "$destination")
git_alias="!f() { $quoted_destination \"\$@\"; }; f"

if [ "$create_git_alias" -eq 1 ]; then
    if existing_alias=$(git config --global --get alias.repo-worktree 2>/dev/null); then
        if [ "$existing_alias" != "$git_alias" ]; then
            fail "Git alias 'repo-worktree' already exists; remove it or rerun with --no-git-alias"
        fi
    else
        alias_status=$?
        [ "$alias_status" -eq 1 ] ||
            fail "could not inspect the global Git configuration"
    fi
fi

system_name=$(uname -s)
machine_name=$(uname -m)
case "$system_name:$machine_name" in
    Linux:x86_64 | Linux:amd64)
        target="x86_64-unknown-linux-musl"
        ;;
    Linux:aarch64 | Linux:arm64)
        target="aarch64-unknown-linux-musl"
        ;;
    Darwin:x86_64 | Darwin:amd64)
        target="x86_64-apple-darwin"
        ;;
    Darwin:arm64 | Darwin:aarch64)
        target="aarch64-apple-darwin"
        ;;
    *)
        fail "unsupported platform: $system_name $machine_name"
        ;;
esac

if [ -n "${GIT_REPO_WORKTREE_RELEASE_BASE:-}" ]; then
    release_base=${GIT_REPO_WORKTREE_RELEASE_BASE%/}
    release_label=$release_version
else
    if [ "$release_version" = "latest" ]; then
        effective_url=$(
            curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
                -o /dev/null -w '%{url_effective}' \
                "$repository_url/releases/latest"
        ) || fail "could not resolve the latest GitHub release"
        release_label=${effective_url##*/}
    else
        release_label=$release_version
        case "$release_label" in
            v*) ;;
            *) release_label=v$release_label ;;
        esac
    fi

    case "$release_label" in
        v[0-9]*)
            case "$release_label" in
                *[!A-Za-z0-9._-]*)
                    fail "invalid release version: $release_label"
                    ;;
            esac
            ;;
        *)
            fail "invalid release version: $release_label"
            ;;
    esac
    release_base="$repository_url/releases/download/$release_label"
fi

asset="git-repo-worktree-$target.tar.gz"
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/git-repo-worktree-install.XXXXXX")
archive="$temporary_dir/$asset"
checksums="$temporary_dir/SHA256SUMS"

say "Downloading git-repo-worktree $release_label for $target..."
if [ -n "${GIT_REPO_WORKTREE_RELEASE_BASE:-}" ]; then
    curl -LsSf "$release_base/$asset" -o "$archive" ||
        fail "could not download $asset"
    curl -LsSf "$release_base/SHA256SUMS" -o "$checksums" ||
        fail "could not download SHA256SUMS"
else
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
        "$release_base/$asset" -o "$archive" ||
        fail "could not download $asset"
    curl --proto '=https' --proto-redir '=https' --tlsv1.2 -LsSf \
        "$release_base/SHA256SUMS" -o "$checksums" ||
        fail "could not download SHA256SUMS"
fi

expected_checksum=$(
    awk -v asset="$asset" '
        $2 == asset || $2 == "*" asset {
            print $1
            found = 1
            exit
        }
        END {
            if (!found) {
                exit 1
            }
        }
    ' "$checksums"
) || fail "SHA256SUMS has no entry for $asset"

case "$expected_checksum" in
    *[!0123456789abcdefABCDEF]* | "")
        fail "invalid checksum for $asset"
        ;;
esac
[ "${#expected_checksum}" -eq 64 ] || fail "invalid checksum for $asset"

if command -v sha256sum >/dev/null 2>&1; then
    checksum_output=$(sha256sum "$archive") ||
        fail "could not checksum $asset"
else
    command -v shasum >/dev/null 2>&1 ||
        fail "neither sha256sum nor shasum is available"
    checksum_output=$(shasum -a 256 "$archive") ||
        fail "could not checksum $asset"
fi
actual_checksum=${checksum_output%% *}

expected_lower=$(printf '%s' "$expected_checksum" | tr 'ABCDEF' 'abcdef')
actual_lower=$(printf '%s' "$actual_checksum" | tr 'ABCDEF' 'abcdef')
[ "$actual_lower" = "$expected_lower" ] ||
    fail "checksum verification failed for $asset"

mkdir "$temporary_dir/extract"
if ! tar -tzf "$archive" |
    awk -v prefix="git-repo-worktree-$target/" '
        {
            entry = $0
            if (entry ~ /^\// || entry ~ /(^|\/)\.\.(\/|$)/) {
                invalid = 1
            }
            if (entry != substr(prefix, 1, length(prefix) - 1) &&
                index(entry, prefix) != 1) {
                invalid = 1
            }
            found = 1
        }
        END {
            if (!found || invalid) {
                exit 1
            }
        }
    '
then
    fail "release archive has an unsafe layout"
fi
if ! tar -tvzf "$archive" |
    awk '
        {
            entry_type = substr($1, 1, 1)
            if (entry_type != "-" && entry_type != "d") {
                invalid = 1
            }
            found = 1
        }
        END {
            if (!found || invalid) {
                exit 1
            }
        }
    '
then
    fail "release archive contains links or special files"
fi
tar -xzf "$archive" -C "$temporary_dir/extract" ||
    fail "could not extract $asset"
source_binary="$temporary_dir/extract/git-repo-worktree-$target/git-repo-worktree"
[ -f "$source_binary" ] && [ ! -L "$source_binary" ] ||
    fail "release archive does not contain a regular git-repo-worktree binary"
chmod +x "$source_binary"
"$source_binary" version >/dev/null ||
    fail "downloaded git-repo-worktree failed its version check"

install_temporary="$install_dir/.git-repo-worktree.install.$$"
cp "$source_binary" "$install_temporary"
chmod 755 "$install_temporary"
mv -f "$install_temporary" "$destination"
install_temporary=""

found_binary=$(command -v git-repo-worktree 2>/dev/null || true)
if [ "$create_git_alias" -eq 1 ] && [ "$found_binary" != "$destination" ]; then
    git config --global alias.repo-worktree "$git_alias" ||
        fail "installed the binary but could not configure the global Git alias"
    alias_configured=1
else
    alias_configured=0
fi

if [ "$create_git_alias" -eq 1 ]; then
    git repo-worktree version >/dev/null ||
        fail "installed the binary but 'git repo-worktree' did not pass verification"
elif [ "$found_binary" != "$destination" ]; then
    warn "$install_dir is not the git-repo-worktree found on PATH"
    warn "add it to PATH before running git repo-worktree"
fi

say "Installed $destination"
if [ "$alias_configured" -eq 1 ]; then
    say "Configured Git so 'git repo-worktree' works without changing PATH."
fi
if [ "$create_git_alias" -eq 1 ] || [ "$found_binary" = "$destination" ]; then
    say "Run: git repo-worktree version"
else
    say "Run after updating PATH: git repo-worktree version"
fi
