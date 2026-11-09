#!/bin/sh
# Builds the app with `make build`, commits with a message from a local model,
# pushes to main, and publishes a GitHub release with the gh CLI. The app reads
# that release to update itself.
#
# Usage: make deploy [VERSION=1.2.3] [LLM_URL=...] [LLM_MODEL=...]
# VERSION defaults to the next patch version.
set -eu

REPO="its-ash/xsh"
LLM_URL="${LLM_URL:-http://127.0.0.1:9694/v1}"
LLM_MODEL="${LLM_MODEL:-router/auto}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

die() {
    echo "deploy: $*" >&2
    exit 1
}

for tool in git gh make curl shasum tar; do
    command -v "$tool" > /dev/null || die "$tool is not installed"
done

# --- Checks that change nothing.
git rev-parse --is-inside-work-tree > /dev/null 2>&1 \
    || die "this folder is not a git repository. Run: git init -b main"
url="$(git config --get remote.origin.url || true)"
case "$url" in
    *"$REPO" | *"$REPO".git) ;;
    *) die "the remote 'origin' must be $REPO (it is '$url'). Run: git remote add origin https://github.com/$REPO.git" ;;
esac
gh auth status > /dev/null 2>&1 || die "GitHub CLI has no login. Run: gh auth login"
curl -fsS -m 5 "$LLM_URL/models" > /dev/null || die "no model answers at $LLM_URL"

branch="$(git symbolic-ref --short HEAD 2> /dev/null || true)"
[ "$branch" = main ] || die "check out main first (you are on '${branch:-a detached HEAD}')"

current="$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)"
version="${VERSION:-$(echo "$current" | awk -F. '{ printf "%d.%d.%d", $1, $2, $3 + 1 }')}"
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "bad version: $version"
tag="v$version"
! git rev-parse -q --verify "refs/tags/$tag" > /dev/null || die "tag $tag already exists"

# --- Until the commit, a failure puts the version files back.
work="$(mktemp -d)"
committed=0
cp Cargo.toml Cargo.lock "$work/"
cleanup() {
    if [ "$committed" = 0 ]; then
        git reset -q 2> /dev/null || true
        cp "$work/Cargo.toml" "$work/Cargo.lock" "$ROOT/"
        echo "deploy: stopped; nothing was pushed" >&2
    fi
    rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# The app's Info.plist takes its version from Cargo.toml, and the updater
# checks that it matches the release tag.
awk -v v="$version" '!done && /^version = / { print "version = \"" v "\""; done = 1; next } { print }' \
    "$work/Cargo.toml" > Cargo.toml

echo "deploy: make build ($tag)"
make build

# The updater downloads xsh-<tag>-<target>.tar.gz and its .sha256.
target="$(uname -m | sed 's/arm64/aarch64/')-apple-darwin"
dist="$ROOT/target/release-assets/$tag"
rm -rf "$dist"
mkdir -p "$dist"
name="xsh-$tag-$target"
mkdir "$work/$name"
cp -R target/xsh.app "$work/$name/"
cp target/xsh.app/Contents/MacOS/xsh "$work/$name/"
tar -C "$work" -czf "$dist/$name.tar.gz" "$name"
(cd "$dist" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")

git add -A
secrets="$(git diff --cached --name-only | grep -Ei '(^|/)(\.env(\..*)?|.*\.(pem|key|p12)|id_rsa|id_ed25519)$' || true)"
[ -z "$secrets" ] || die "these staged files can hold secrets: $secrets"

echo "deploy: ask $LLM_MODEL at $LLM_URL for the commit message"
message="$(
    { git diff --cached --stat; git diff --cached -- . ':(exclude)Cargo.lock'; } \
        | target/xsh.app/Contents/MacOS/xsh app commit-message --endpoint "$LLM_URL" --model "$LLM_MODEL"
)"
[ -n "$message" ] || die "the model gave no commit message"
echo "deploy: $message"

git commit -q -m "$message"
git tag -a "$tag" -m "$tag"
committed=1

git push --atomic origin main "$tag" \
    || die "the push failed. Commit and tag $tag are local. Fix the cause, then run: git push --atomic origin main $tag"
gh release create "$tag" "$dist"/* --repo "$REPO" --title "$tag" --generate-notes --verify-tag \
    || die "the release failed. The push is done. Run: gh release create $tag $dist/* --repo $REPO --title $tag --generate-notes --verify-tag"
echo "deploy: published $tag"
