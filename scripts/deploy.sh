#!/bin/sh
# Builds a release, commits with a message from a local model, pushes to main,
# and publishes a GitHub release. The app reads that release to update itself.
#
# Usage: make deploy [VERSION=1.2.3] [LLM_URL=...] [LLM_MODEL=...]
# VERSION defaults to the next patch version.
set -eu

REPO="its-ash/rushy"
TARGETS="x86_64-apple-darwin aarch64-apple-darwin"
LLM_URL="${LLM_URL:-http://127.0.0.1:9694/v1}"
LLM_MODEL="${LLM_MODEL:-router/auto}"

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

die() {
    echo "deploy: $*" >&2
    exit 1
}

for tool in git gh cargo swift curl shasum tar; do
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
visibility="$(gh repo view "$REPO" --json visibility --jq .visibility 2> /dev/null || true)"
[ -n "$visibility" ] || die "$REPO does not exist on GitHub. Run: gh repo create $REPO --public"
# The app downloads releases with no login.
[ "$visibility" = PUBLIC ] || die "$REPO is $visibility; auto-update needs a public repository"
curl -fsS -m 5 "$LLM_URL/models" > /dev/null || die "no model answers at $LLM_URL"

if git rev-parse -q --verify refs/heads/main > /dev/null; then
    git checkout main
elif git rev-parse -q --verify HEAD > /dev/null; then
    die "there is no branch named main"
fi
if git ls-remote --exit-code --heads origin main > /dev/null 2>&1; then
    git fetch -q origin main
    git rev-parse -q --verify HEAD > /dev/null \
        && git merge-base --is-ancestor origin/main HEAD \
        || die "origin/main has commits that are not here. Run: git pull"
fi

current="$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)"
version="${VERSION:-$(echo "$current" | awk -F. '{ printf "%d.%d.%d", $1, $2, $3 + 1 }')}"
echo "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die "bad version: $version"
tag="v$version"
! git rev-parse -q --verify "refs/tags/$tag" > /dev/null || die "tag $tag already exists"
[ -z "$(git ls-remote --tags origin "$tag")" ] || die "tag $tag already exists on GitHub"

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

awk -v v="$version" '!done && /^version = / { print "version = \"" v "\""; done = 1; next } { print }' \
    "$work/Cargo.toml" > Cargo.toml

echo "deploy: checks for $tag"
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

dist="$ROOT/target/release-assets/$tag"
rm -rf "$dist"
mkdir -p "$dist"
for target in $TARGETS; do
    echo "deploy: build $target"
    sh app/bundle.sh release "$target"
    name="xsh-$tag-$target"
    stage="$work/$name"
    mkdir "$stage"
    cp -R target/xsh.app "$stage/"
    cp "target/$target/release/xsh" "$stage/"
    cp -R plugins "$stage/plugins"
    cp README.md CHANGELOG.md LICENSE-APACHE LICENSE-MIT registry.json "$stage/"
    tar -C "$work" -czf "$dist/$name.tar.gz" "$name"
    (cd "$dist" && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
done

git add -A
secrets="$(git diff --cached --name-only | grep -Ei '(^|/)(\.env(\..*)?|.*\.(pem|key|p12)|id_rsa|id_ed25519)$' || true)"
[ -z "$secrets" ] || die "these staged files can hold secrets: $secrets"

host="$(uname -m | sed 's/arm64/aarch64/')-apple-darwin"
echo "deploy: ask $LLM_MODEL for the commit message"
message="$(
    { git diff --cached --stat; git diff --cached -- . ':(exclude)Cargo.lock'; } \
        | "target/$host/release/xsh" app commit-message --endpoint "$LLM_URL" --model "$LLM_MODEL"
)"
[ -n "$message" ] || die "the model gave no commit message"
echo "deploy: $message"

git commit -q -m "$message"
git tag -a "$tag" -m "$tag"
committed=1

git push --atomic origin main "$tag" || die "the push failed. Commit and tag $tag are local. Fix the cause, then run: git push --atomic origin main $tag"
gh release create "$tag" "$dist"/* --repo "$REPO" --title "$tag" --generate-notes --verify-tag \
    || die "the release failed. The push is done. Run: gh release create $tag $dist/* --repo $REPO --title $tag --generate-notes --verify-tag"
echo "deploy: published $tag"
