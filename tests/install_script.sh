#!/bin/sh

set -eu

project_root=$(CDPATH='' cd "$(dirname "$0")/.." && pwd -P)
temporary_dir=$(mktemp -d "${TMPDIR:-/tmp}/git-repo-worktree-installer-test.XXXXXX")
installer_shell="${INSTALLER_TEST_SHELL:-sh}"

cleanup() {
    rm -rf "$temporary_dir"
}
trap cleanup 0
trap 'exit 1' 1 2 15

case "$(uname -s):$(uname -m)" in
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
        printf 'installer test does not support this host\n' >&2
        exit 1
        ;;
esac

release_dir="$temporary_dir/release"
package="git-repo-worktree-$target"
mkdir -p "$release_dir/$package"

cat >"$release_dir/$package/git-repo-worktree" <<'EOF'
#!/bin/sh
if [ "${1:-}" = "version" ]; then
    printf 'git-repo-worktree test\n'
    exit 0
fi
if [ -n "${GIT_REPO_WORKTREE_TEST_LOG:-}" ]; then
    : >"$GIT_REPO_WORKTREE_TEST_LOG"
    for argument in "$@"; do
        printf '<%s>\n' "$argument" >>"$GIT_REPO_WORKTREE_TEST_LOG"
    done
fi
EOF
chmod +x "$release_dir/$package/git-repo-worktree"
printf '.TH GIT-REPO-WORKTREE 1\n' >"$release_dir/$package/git-repo-worktree.1"
printf '.TH GIT-REPO-WORKTREE-ADD 1\n' >"$release_dir/$package/git-repo-worktree-add.1"
tar -czf "$release_dir/$package.tar.gz" -C "$release_dir" "$package"

if command -v sha256sum >/dev/null 2>&1; then
    checksum=$(sha256sum "$release_dir/$package.tar.gz")
else
    checksum=$(shasum -a 256 "$release_dir/$package.tar.gz")
fi
checksum=${checksum%% *}
printf '%s  %s.tar.gz\n' "$checksum" "$package" >"$release_dir/SHA256SUMS"

PATH="$(dirname "$(command -v git)"):/usr/bin:/bin:/usr/sbin:/sbin"
export PATH

test_home="$temporary_dir/home"
install_dir="$temporary_dir/install dir's bin"
mkdir "$test_home"
export HOME="$test_home"
export GIT_CONFIG_NOSYSTEM=1
export GIT_REPO_WORKTREE_RELEASE_BASE="file://$release_dir"

"$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 \
    --install-dir "$install_dir"

test -x "$install_dir/git-repo-worktree"
git repo-worktree version >/dev/null
test -f "$test_home/.local/share/man/man1/git-repo-worktree.1"
bin_layout="$temporary_dir/prefix/bin"
HOME="$test_home" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 --install-dir "$bin_layout" --no-git-alias >/dev/null 2>&1
test -f "$temporary_dir/prefix/share/man/man1/git-repo-worktree.1"

argument_log="$temporary_dir/arguments"
export GIT_REPO_WORKTREE_TEST_LOG="$argument_log"
git repo-worktree alpha "two words"
expected_arguments=$(printf '<alpha>\n<two words>')
actual_arguments=$(cat "$argument_log")
test "$actual_arguments" = "$expected_arguments"

configured_alias=$(git config --global --get alias.repo-worktree)
case "$configured_alias" in
    *git-repo-worktree*) ;;
    *)
        printf 'configured alias does not point at the installed binary\n' >&2
        exit 1
        ;;
esac

default_home="$temporary_dir/default-home"
mkdir "$default_home"
HOME="$default_home" "$installer_shell" "$project_root/install.sh" --version v9.9.9
test -x "$default_home/.local/bin/git-repo-worktree"
HOME="$default_home" git repo-worktree version >/dev/null
test -f "$default_home/.local/share/man/man1/git-repo-worktree.1"
test -f "$default_home/.local/share/man/man1/git-repo-worktree-add.1"

shadow_home="$temporary_dir/shadow-home"
shadow_bin="$temporary_dir/shadow-bin"
mkdir "$shadow_home" "$shadow_bin"
# A stale copy that reports the same version string must still be noticed.
printf '%s\n' '#!/bin/sh' '# older build' "printf 'git-repo-worktree test\\n'" >"$shadow_bin/git-repo-worktree"
chmod +x "$shadow_bin/git-repo-worktree"
HOME="$shadow_home" PATH="$shadow_bin:$PATH" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 --no-man >"$temporary_dir/shadow-output" 2>&1
grep -q "shadows .*shadow-home/.local/bin/git-repo-worktree" "$temporary_dir/shadow-output"
test ! -e "$shadow_home/.local/share/man/man1/git-repo-worktree.1"

cp "$shadow_home/.local/bin/git-repo-worktree" "$shadow_bin/git-repo-worktree"
HOME="$shadow_home" PATH="$shadow_bin:$PATH" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 --no-man >"$temporary_dir/identical-output" 2>&1
if grep -q "shadows" "$temporary_dir/identical-output"; then
    printf 'installer warned about an identical copy\n' >&2
    exit 1
fi

no_alias_home="$temporary_dir/no-alias-home"
no_alias_dir="$temporary_dir/no-alias-bin"
mkdir "$no_alias_home"
HOME="$no_alias_home" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 \
    --install-dir "$no_alias_dir" \
    --no-git-alias
test -x "$no_alias_dir/git-repo-worktree"
if HOME="$no_alias_home" git config --global --get alias.repo-worktree >/dev/null 2>&1; then
    printf '%s\n' '--no-git-alias unexpectedly configured an alias' >&2
    exit 1
fi

conflict_home="$temporary_dir/conflict-home"
mkdir "$conflict_home"
HOME="$conflict_home" git config --global alias.repo-worktree '!echo existing'
if HOME="$conflict_home" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 \
    --install-dir "$temporary_dir/conflict-bin" \
    >"$temporary_dir/conflict-output" 2>&1; then
    printf 'installer overwrote a conflicting alias\n' >&2
    exit 1
fi
test "$(HOME="$conflict_home" git config --global --get alias.repo-worktree)" = '!echo existing'

printf '%064d  %s.tar.gz\n' 0 "$package" >"$release_dir/SHA256SUMS"
checksum_home="$temporary_dir/checksum-home"
mkdir "$checksum_home"
if HOME="$checksum_home" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 \
    --install-dir "$temporary_dir/checksum-bin" \
    >"$temporary_dir/checksum-output" 2>&1; then
    printf 'installer accepted a bad checksum\n' >&2
    exit 1
fi
grep -q 'checksum verification failed' "$temporary_dir/checksum-output"

unsafe_dir="$temporary_dir/unsafe"
mkdir "$unsafe_dir"
printf '%s\n' '#!/bin/sh' 'exit 0' >"$unsafe_dir/outside"
chmod +x "$unsafe_dir/outside"
tar -czf "$release_dir/$package.tar.gz" -C "$unsafe_dir" outside
if command -v sha256sum >/dev/null 2>&1; then
    checksum=$(sha256sum "$release_dir/$package.tar.gz")
else
    checksum=$(shasum -a 256 "$release_dir/$package.tar.gz")
fi
checksum=${checksum%% *}
printf '%s  %s.tar.gz\n' "$checksum" "$package" >"$release_dir/SHA256SUMS"
unsafe_home="$temporary_dir/unsafe-home"
mkdir "$unsafe_home"
if HOME="$unsafe_home" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 \
    --install-dir "$temporary_dir/unsafe-bin" \
    >"$temporary_dir/unsafe-output" 2>&1; then
    printf 'installer accepted an unsafe archive layout\n' >&2
    exit 1
fi
grep -q 'release archive has an unsafe layout' "$temporary_dir/unsafe-output"

link_dir="$temporary_dir/link"
mkdir -p "$link_dir/$package"
ln -s /bin/true "$link_dir/$package/git-repo-worktree"
tar -czf "$release_dir/$package.tar.gz" -C "$link_dir" "$package"
if command -v sha256sum >/dev/null 2>&1; then
    checksum=$(sha256sum "$release_dir/$package.tar.gz")
else
    checksum=$(shasum -a 256 "$release_dir/$package.tar.gz")
fi
checksum=${checksum%% *}
printf '%s  %s.tar.gz\n' "$checksum" "$package" >"$release_dir/SHA256SUMS"
link_home="$temporary_dir/link-home"
mkdir "$link_home"
if HOME="$link_home" "$installer_shell" "$project_root/install.sh" \
    --version v9.9.9 \
    --install-dir "$temporary_dir/link-bin" \
    >"$temporary_dir/link-output" 2>&1; then
    printf 'installer accepted a linked binary in the archive\n' >&2
    exit 1
fi
grep -q 'release archive contains links or special files' "$temporary_dir/link-output"

if env -u HOME -u GIT_REPO_WORKTREE_INSTALL_DIR \
    "$installer_shell" "$project_root/install.sh" \
    >"$temporary_dir/no-home-output" 2>&1; then
    printf 'installer accepted a missing HOME without --install-dir\n' >&2
    exit 1
fi
grep -q 'HOME is unset' "$temporary_dir/no-home-output"

printf 'installer tests passed\n'
