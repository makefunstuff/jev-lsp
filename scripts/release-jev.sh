#!/usr/bin/env bash
#
# What a release of jev-lsp is, in one file. `.github/workflows/release.yml` runs this script
# rather than re-implementing any of it, so a release made on this machine and one made on a
# runner cannot produce different assets.
#
#   scripts/release-jev.sh all [--out DIR] [--publish] [--built-by TEXT]
#       The route a person takes: the guards below, then the host build, the extension, the
#       checksums and the notes, then the release itself — and only with `--publish`. Without
#       that flag the commands are printed and nothing public is touched.
#   scripts/release-jev.sh build [--target TRIPLE] [--out DIR]
#       One platform's tarball, `jev-lsp-vX.Y.Z-TRIPLE.tar.gz`, holding `jev-lsp` and `jev` at
#       the archive root. Without `--target` it builds the host; with one it builds that rustup
#       target (`rustup target add TRIPLE` first if it is not installed).
#   scripts/release-jev.sh vsix [--out DIR]
#       The editor extension, `jev-X.Y.Z.vsix`: one platform-independent asset serving Cursor and
#       VS Code, packed by `editors/cursor/pack.sh`, which needs `bash`, `jq`, `zip`, `unzip`.
#   scripts/release-jev.sh assemble --dir DIR [--built-by TEXT]
#       Fills DIR: `SHA256SUMS` over every asset in it, and `RELEASE-NOTES.md` — the tag's
#       annotation, then what the assets are, where they were built and how to get them elsewhere.
#   scripts/release-jev.sh publish --dir DIR [--publish] [--built-by TEXT]
#       The release: the tag if it is missing, then `gh release create` if the release is missing
#       or `gh release upload --clobber` if it is not. Idempotent by construction; it never
#       deletes a release, a tag or an asset, and it never edits notes that are already published.
#   scripts/release-jev.sh version [--tag]
#       The version, or the tag name, from the one file that owns it.
#
# DIR is the asset set: every file in it is uploaded, except `RELEASE-NOTES.md`, which is the
# release's note text and never an asset.
#
# The version. `[workspace.package] version` in `Cargo.toml` owns it: that is the string the
# binaries print (`jev-lsp --version`) and the string the asset names carry.
# `editors/cursor/package.json` carries the extension's own version, which the editor requires
# independently, and this script refuses to release while the two disagree. The
# `makefunstuff.jev-<version>` symlink name in the documentation is prose; nothing checks it.
#
# The guards, and where they apply. `all` refuses a release tree that is dirty, a HEAD that is not
# `origin/main`, and a version that is already tagged or released — the three ways a person's local
# release goes wrong. `publish` is the verb CI runs, on a tag that already exists, and it is
# idempotent instead: an existing tag is reused, an existing release takes the assets again with
# `--clobber`, and nothing is ever deleted.
#
# No `set -e` is not used here on purpose: every step is checked, and the checks are the point.

set -euo pipefail

HERE="$(cd "$(dirname "$(realpath "$0")")" && pwd)"
REPO="$(cd "$HERE/.." && pwd)"
cd "$REPO"

PROG="release-jev"
PUBLISH=0
OUT=""
DIR=""
TARGET=""
BUILT_BY=""

die() { echo "$PROG: $*" >&2; exit 1; }
say() { echo "$PROG: $*" >&2; }

# --- the facts the rest of the script is built on ---------------------------------------------

# The version owner, scoped to the table so a dependency's `version = "1"` can never be read.
workspace_version() {
  sed -n '/^\[workspace\.package\]/,/^\[/p' "$REPO/Cargo.toml" \
    | sed -n 's/^version *= *"\([^"]*\)".*$/\1/p' | head -1
}

extension_version() {
  sed -n 's/^ *"version": *"\([^"]*\)".*$/\1/p' "$REPO/editors/cursor/package.json" | head -1
}

host_triple() { rustc -vV | sed -n 's/^host: //p'; }

version_or_die() {
  local v; v="$(workspace_version)"
  [ -n "$v" ] || die "no version found in [workspace.package] of Cargo.toml"
  printf '%s\n' "$v"
}

tag_or_die() { printf 'v%s\n' "$(version_or_die)"; }

# The one place a version disagreement is caught, and it is caught before anything is built.
require_version_agreement() {
  local manifest ext
  manifest="$(version_or_die)"
  ext="$(extension_version)"
  [ -n "$ext" ] || die "no version found in editors/cursor/package.json"
  [ "$ext" = "$manifest" ] || die "Cargo.toml says $manifest and editors/cursor/package.json says $ext; the extension and the binaries must agree before a release"
  say "version $manifest (Cargo.toml owns it; package.json agrees)"
}

# `owner/repo`, from either `git@github.com:owner/repo.git` or a host alias in ssh config
# (`github-makefunstuff:owner/repo`) or an https URL. Passed to every `gh` call explicitly, so
# nothing depends on gh's own guess about the remote — the alias form is not one it resolves.
repo_slug() {
  local url slug
  url="$(git remote get-url origin)"
  case "$url" in
    *://*) slug="${url#*://}"; slug="${slug#*/}" ;;
    *@*:*) slug="${url#*:}" ;;
    *:*)   slug="${url#*:}" ;;
    *)     slug="$url" ;;
  esac
  slug="${slug%.git}"
  case "$slug" in
    */*) printf '%s\n' "$slug" ;;
    *)   die "cannot read owner/repo from origin ($url)" ;;
  esac
}

sha_tool() { if command -v shasum >/dev/null 2>&1; then echo "shasum -a 256"; else echo "sha256sum"; fi; }

# Every path this script hands to another program is absolute. `editors/cursor/pack.sh` zips from
# inside a temporary directory of its own, so a *relative* output path lands there and is removed
# with the directory — the bug this script shipped with, which the first dispatch of release.yml
# caught and nothing local did, because a hand-run used an absolute `--out`.
absolute() { ( cd "$1" && pwd ); }

# Basenames of the assets in $DIR, one per line, sorted. `SHA256SUMS` and `RELEASE-NOTES.md` are
# not assets; everything else in the directory is.
asset_names() {
  ( cd "$DIR" && find . -maxdepth 1 -type f ! -name 'SHA256SUMS' ! -name 'RELEASE-NOTES.md' -print ) \
    | sed 's|^\./||' | LC_ALL=C sort
}

require_assets() {
  local names; names="$(asset_names)"
  [ -n "$names" ] || die "$DIR holds no assets"
  # Every consumer below passes these names through an unquoted expansion or an argument list;
  # names this script generates never contain spaces, and this is the check that keeps that true.
  printf '%s\n' "$names" | while IFS= read -r n; do
    case "$n" in *[[:space:]]*) die "asset name contains whitespace: $n" ;; esac
  done
}

# --- the guards `all` applies ----------------------------------------------------------------

# The release inputs, and only those. Another session editing `docs/**` or `nvim/**` is not a
# reason to refuse a release: those files are in neither the tarball nor the extension. They are
# named in the output instead, so the person running this can see what was left out rather than
# wonder whether the check ran.
RELEASE_PATHS="Cargo.toml Cargo.lock crates editors/cursor scripts"

guard_clean_release_tree() {
  local dirty ignored
  dirty="$(git status --porcelain -- $RELEASE_PATHS)"
  if [ -n "$dirty" ]; then
    echo "$dirty" >&2
    die "the release inputs above are not committed; commit them or revert them first"
  fi
  ignored="$(git status --porcelain | cut -c4- \
    | grep -v -E '^(Cargo\.toml|Cargo\.lock|crates/|editors/cursor/|scripts/)' || true)"
  if [ -n "$ignored" ]; then
    say "not part of a release, ignored:"
    printf '  %s\n' $ignored >&2
  fi
}

guard_head_pushed() {
  git fetch --quiet origin main || die "cannot fetch origin/main"
  local head upstream
  head="$(git rev-parse HEAD)"
  upstream="$(git rev-parse origin/main)"
  [ "$head" = "$upstream" ] \
    || die "HEAD is $head and origin/main is $upstream; push before releasing, or the tag will name a commit nobody can fetch"
}

guard_tag_absent() {
  local tag; tag="$(tag_or_die)"
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    die "$tag is already a tag here; a released version is not re-released from a working copy (use `publish` if you are repairing its assets)"
  fi
  if git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
    die "$tag is already a tag on origin; a released version is not re-released from a working copy"
  fi
}

guard_release_absent() {
  local tag slug; tag="$(tag_or_die)"; slug="$(repo_slug)"
  if gh release view "$tag" --repo "$slug" >/dev/null 2>&1; then
    die "$tag is already a published release at https://github.com/$slug/releases/tag/$tag; assets are repaired with `publish`, not re-released"
  fi
}

# --- the three things a release is made of ----------------------------------------------------

cmd_build() {
  local tag target bindir tarball
  tag="$(tag_or_die)"
  target="${TARGET:-$(host_triple)}"
  [ -n "$OUT" ] || OUT="$REPO/target/dist/$tag"
  mkdir -p "$OUT"
  OUT="$(absolute "$OUT")"

  if [ "$target" = "$(host_triple)" ]; then
    say "building $target (host)"
    cargo build --release --locked
    bindir="$REPO/target/release"
  else
    say "building $target (rustup target)"
    rustup target list --installed | grep -qx "$target" \
      || die "$target is not installed; run: rustup target add $target"
    cargo build --release --locked --target "$target"
    bindir="$REPO/target/$target/release"
  fi

  [ -x "$bindir/jev-lsp" ] || die "no jev-lsp at $bindir"
  [ -x "$bindir/jev" ] || die "no jev at $bindir"

  tarball="$OUT/jev-lsp-$tag-$target.tar.gz"
  rm -f "$tarball"
  # COPYFILE_DISABLE keeps macOS tar from writing AppleDouble `._` members into the archive, and
  # ustar is the format both bsdtar and GNU tar write the same way. A release that unpacks
  # differently depending on who packed it is not a release.
  COPYFILE_DISABLE=1 tar --format=ustar -czf "$tarball" -C "$bindir" jev-lsp jev
  say "packed $(basename "$tarball") ($(wc -c < "$tarball" | tr -d ' ') bytes)"
  tar -tzf "$tarball" | sed 's/^/    /' >&2
}

cmd_vsix() {
  local tag
  tag="$(tag_or_die)"
  [ -n "$OUT" ] || OUT="$REPO/target/dist/$tag"
  mkdir -p "$OUT"
  OUT="$(absolute "$OUT")"
  # pack.sh writes from package.json: name, publisher, version, display name, description, engine.
  bash "$REPO/editors/cursor/pack.sh" "$OUT/jev-$(version_or_die).vsix" >/dev/null
  say "packed jev-$(version_or_die).vsix ($(wc -c < "$OUT/jev-$(version_or_die).vsix" | tr -d ' ') bytes)"
}

# The asset table is derived from what is in DIR, so a release whose matrix built one platform
# says one platform rather than claiming what it does not carry.
platform_of() {
  case "$1" in
    *aarch64-apple-darwin*)      echo "macOS on arm64 (Apple silicon)" ;;
    *x86_64-apple-darwin*)       echo "macOS on x86_64 (Intel)" ;;
    *x86_64-unknown-linux-gnu*)  echo "Linux on x86_64 (glibc)" ;;
    *aarch64-unknown-linux-gnu*) echo "Linux on arm64 (glibc)" ;;
    *x86_64-pc-windows-msvc*)    echo "Windows on x86_64" ;;
    *.vsix)                      echo "Cursor and VS Code, any platform" ;;
    SHA256SUMS)                  echo "checksums for the files above" ;;
    *)                           echo "unrecognised platform" ;;
  esac
}

cmd_assemble() {
  local tag names notes sha
  tag="$(tag_or_die)"
  [ -n "$DIR" ] || die "assemble needs --dir DIR"
  [ -d "$DIR" ] || die "$DIR is not a directory"
  DIR="$(absolute "$DIR")"
  require_assets
  names="$(asset_names)"
  sha="$(sha_tool)"
  notes="$DIR/RELEASE-NOTES.md"

  # The tag's annotation is the release note; when there is none, `publish` asks GitHub for its
  # own generated notes instead of inventing a changelog format this repository does not use.
  local annotation="" built_by="$BUILT_BY"
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    annotation="$(git tag -l --format='%(contents)' "$tag" | awk '
      { line[NR] = $0 }
      END { last = NR
            while (last > 0 && line[last] ~ /^[[:space:]]*$/) last--
            for (i = 1; i <= last; i++) print line[i] }')"
  fi
  [ -n "$built_by" ] || built_by="by hand, on one machine ($(host_triple)), from this checkout, with no CI"

  local platforms
  platforms="$(printf '%s\n' "$names" | while IFS= read -r n; do
      case "$n" in *.vsix|SHA256SUMS) continue ;; esac
      platform_of "$n"
    done | LC_ALL=C sort -u)"
  local platform_sentence
  if [ -z "$platforms" ]; then
    platform_sentence="No platform archive is in this release."
  else
    platform_sentence="Only $(printf '%s' "$platforms" | tr '\n' ',' | sed 's/,$//; s/,/, /g') was built."
  fi

  {
    if [ -n "$annotation" ]; then
      printf '%s\n\n' "$annotation"
    fi
    echo "## What this release carries"
    echo
    echo "| asset | what it is |"
    echo "|---|---|"
    printf '%s\n' "$names" SHA256SUMS | while IFS= read -r n; do
      printf '| `%s` | %s |\n' "$n" "$(platform_of "$n")"
    done
    echo
    echo "Built $built_by."
    echo
    echo "$platform_sentence Nothing was built for any other platform. On one of them:"
    echo
    echo '```sh'
    echo "cargo install --git https://github.com/makefunstuff/jev-lsp --locked jev-lsp jev"
    echo '```'
    echo
    echo "That needs a Rust toolchain (1.75 or later) and takes about two minutes."
    echo
    echo "Check what you downloaded before you run it: \`shasum -a 256 -c SHA256SUMS\` on macOS,"
    echo "\`sha256sum -c SHA256SUMS\` on Linux."
    echo
    echo "This release grants no licence: the repository carries no \`LICENSE\` file and its"
    echo "manifests declare no licence."
  } > "$notes"

  ( cd "$DIR" && $sha $names > SHA256SUMS )
  say "wrote $notes"
  say "wrote $DIR/SHA256SUMS"
  sed 's/^/    /' "$DIR/SHA256SUMS" >&2
}

# --- the release itself ----------------------------------------------------------------------

print_cmd() {
  local out="" a
  for a in "$@"; do out="$out $(printf '%q' "$a")"; done
  printf '  %s\n' "${out# }"
}

cmd_publish() {
  local tag slug names annotation notes
  tag="$(tag_or_die)"
  slug="$(repo_slug)"
  [ -n "$DIR" ] || die "publish needs --dir DIR"
  [ -d "$DIR" ] || die "$DIR is not a directory"
  DIR="$(absolute "$DIR")"
  [ -f "$DIR/SHA256SUMS" ] || die "$DIR has no SHA256SUMS; run assemble first"
  [ -f "$DIR/RELEASE-NOTES.md" ] || cmd_assemble
  require_assets
  names="$(asset_names)"
  notes="$DIR/RELEASE-NOTES.md"
  annotation=""
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    annotation="$(git tag -l --format='%(contents)' "$tag" | awk 'NF { found = 1 } { if (found) print }')"
  fi

  local tag_present=0
  if git rev-parse -q --verify "refs/tags/$tag" >/dev/null \
    || git ls-remote --exit-code --tags origin "refs/tags/$tag" >/dev/null 2>&1; then
    tag_present=1
  fi

  local released=0
  if gh release view "$tag" --repo "$slug" >/dev/null 2>&1; then released=1; fi

  say "tag $tag: $([ "$tag_present" = 1 ] && echo 'exists' || echo 'will be created')"
  say "release https://github.com/$slug/releases/tag/$tag: $([ "$released" = 1 ] && echo 'exists, assets will be re-uploaded with --clobber' || echo 'will be created')"

  if [ "$PUBLISH" != 1 ]; then
    echo
    echo "$PROG: dry run — the release is not touched. Commands that would run:"
    if [ "$tag_present" = 0 ]; then
      print_cmd git tag -a "$tag" -m "jev-lsp ${tag#v}"
      print_cmd git push origin "$tag"
    fi
    if [ "$released" = 0 ]; then
      # `--verify-tag`, so gh never invents a tag from the default branch: the tag above is the
      # only thing a release may be built from.
      if [ -n "$annotation" ]; then
        print_cmd gh release create "$tag" --repo "$slug" --verify-tag \
          --title "jev-lsp $tag" --notes-file "$notes" $names
      else
        # Documented by `gh release create --help`: "Additional release notes can be prepended to
        # automatically generated notes by using the --notes flag." The tag carries no annotation
        # here, so GitHub's own notes are the note and the platform block is prepended to them.
        print_cmd gh release create "$tag" --repo "$slug" --verify-tag \
          --title "jev-lsp $tag" --generate-notes --notes "$(cat "$notes")" $names
      fi
    else
      print_cmd gh release upload "$tag" --repo "$slug" --clobber $names
    fi
    echo
    echo "$PROG: re-run with --publish to run them."
    return 0
  fi

  if [ "$tag_present" = 0 ]; then
    git tag -a "$tag" -m "jev-lsp ${tag#v}"
    git push origin "$tag"
    say "created and pushed tag $tag"
  fi

  if [ "$released" = 0 ]; then
    if [ -n "$annotation" ]; then
      gh release create "$tag" --repo "$slug" --verify-tag \
        --title "jev-lsp $tag" --notes-file "$notes" $names
    else
      gh release create "$tag" --repo "$slug" --verify-tag \
        --title "jev-lsp $tag" --generate-notes --notes "$(cat "$notes")" $names
    fi
    say "created https://github.com/$slug/releases/tag/$tag"
  else
    gh release upload "$tag" --repo "$slug" --clobber $names
    say "uploaded ${names//$'\n'/ } to the existing release (notes left as published)"
  fi
}

cmd_all() {
  local tag
  require_version_agreement
  guard_clean_release_tree
  guard_head_pushed
  guard_tag_absent
  guard_release_absent
  tag="$(tag_or_die)"
  [ -n "$OUT" ] || OUT="$REPO/target/dist/$tag"
  mkdir -p "$OUT"
  OUT="$(absolute "$OUT")"
  DIR="$OUT"
  cmd_build
  cmd_vsix
  cmd_assemble
  echo >&2
  cmd_publish
}

# --- argument handling ------------------------------------------------------------------------

usage() {
  cat >&2 <<'USAGE'
usage: scripts/release-jev.sh <verb> [flags]

  all      [--out DIR] [--publish] [--built-by TEXT]   guards, host build, vsix, checksums, release
  build    [--target TRIPLE] [--out DIR]               one platform's tarball
  vsix     [--out DIR]                                 the extension, for Cursor and VS Code
  assemble --dir DIR [--built-by TEXT]                 SHA256SUMS and RELEASE-NOTES.md
  publish  --dir DIR [--publish] [--built-by TEXT]     tag and release; dry run without --publish
  version [--tag]                                      the version, or the tag name

DIR holds the assets. RELEASE-NOTES.md is the release's note text, never an asset; everything
else in DIR is uploaded. See the header of this script for what each guard refuses.
USAGE
}

[ $# -ge 1 ] || { usage; exit 2; }
verb="$1"; shift
case "$verb" in
  -h|--help) usage; exit 0 ;;
esac

# Before the flag loop, so `--tag` is not read as an unknown flag of some other verb.
if [ "$verb" = version ]; then
  case "${1:-}" in
    --tag)  [ $# -eq 1 ] || die "version takes no arguments after --tag"; tag_or_die; exit 0 ;;
    "")     version_or_die; exit 0 ;;
    *)      die "version takes no arguments" ;;
  esac
fi

while [ $# -gt 0 ]; do
  case "$1" in
    --out)      [ $# -ge 2 ] || die "--out needs a directory"; OUT="$2"; shift 2 ;;
    --dir)      [ $# -ge 2 ] || die "--dir needs a directory"; DIR="$2"; shift 2 ;;
    --target)   [ $# -ge 2 ] || die "--target needs a triple"; TARGET="$2"; shift 2 ;;
    --built-by) [ $# -ge 2 ] || die "--built-by needs text"; BUILT_BY="$2"; shift 2 ;;
    --publish)  PUBLISH=1; shift ;;
    -h|--help)  usage; exit 0 ;;
    *)          die "unknown argument: $1" ;;
  esac
done

case "$verb" in
  all)      cmd_all ;;
  build)    require_version_agreement; cmd_build ;;
  vsix)     require_version_agreement; cmd_vsix ;;
  assemble) require_version_agreement; cmd_assemble ;;
  publish)  require_version_agreement; cmd_publish ;;
  *) usage; die "unknown verb: $verb" ;;
esac
