#!/usr/bin/env bash
# Build a .vsix from this directory with `zip` alone.
#
#   bash editors/cursor/pack.sh [output.vsix]
#
# A VSIX is a ZIP holding `[Content_Types].xml`, `extension.vsixmanifest` and the extension
# itself under `extension/`. `vsce package` builds the same thing but wants npm and a
# download; this extension has no dependencies, so neither does its packaging.
set -eu

HERE="$(cd "$(dirname "$0")" && pwd)"
NAME="$(jq -r .name "$HERE/package.json")"
PUBLISHER="$(jq -r .publisher "$HERE/package.json")"
VERSION="$(jq -r .version "$HERE/package.json")"
DISPLAY="$(jq -r .displayName "$HERE/package.json")"
DESCRIPTION="$(jq -r .description "$HERE/package.json")"
ENGINE="$(jq -r '.engines.vscode' "$HERE/package.json")"
OUT="${1:-$HERE/$NAME-$VERSION.vsix}"

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/extension"
cp "$HERE/package.json" "$HERE/extension.js" "$HERE/README.md" "$STAGE/extension/"
cp -R "$HERE/media" "$STAGE/extension/media"

# The licence travels in the archive, because a VSIX is what a reader may have and nothing else:
# without this the terms are only discoverable by finding the repository. Two copies, one in each
# place that is looked for. `extension/LICENSE` is where `vsce package` puts it and where VS Code
# reads it for an installed extension; `LICENSE` at the archive root is where a reader unzipping
# the file looks first. A repository without one is packed without one rather than failing.
ROOT="$(cd "$HERE/../.." && pwd)"
if [ -f "$ROOT/LICENSE" ]; then
  cp "$ROOT/LICENSE" "$STAGE/extension/LICENSE"
  cp "$ROOT/LICENSE" "$STAGE/LICENSE"
else
  echo "pack.sh: no LICENSE at $ROOT, packing without one" >&2
fi

cat > "$STAGE/[Content_Types].xml" <<'XML'
<?xml version="1.0" encoding="utf-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
  <Default Extension=".js" ContentType="application/javascript"/>
  <Default Extension=".json" ContentType="application/json"/>
  <Default Extension=".md" ContentType="text/markdown"/>
  <Default Extension=".vsixmanifest" ContentType="text/xml"/>
</Types>
XML

cat > "$STAGE/extension.vsixmanifest" <<XML
<?xml version="1.0" encoding="utf-8"?>
<PackageManifest Version="2.0.0" xmlns="http://schemas.microsoft.com/developer/vsx-schema/2011">
  <Metadata>
    <Identity Language="en-US" Id="$NAME" Version="$VERSION" Publisher="$PUBLISHER"/>
    <DisplayName>$DISPLAY</DisplayName>
    <Description xml:space="preserve">$DESCRIPTION</Description>
    <Tags>linter,language-server</Tags>
    <Categories>Linters,Programming Languages</Categories>
    <GalleryFlags>Public</GalleryFlags>
    <Properties>
      <Property Id="Microsoft.VisualStudio.Code.Engine" Value="$ENGINE"/>
      <Property Id="Microsoft.VisualStudio.Code.ExtensionDependencies" Value=""/>
    </Properties>
  </Metadata>
  <Installation>
    <InstallationTarget Id="Microsoft.VisualStudio.Code"/>
  </Installation>
  <Dependencies/>
  <Assets>
    <Asset Type="Microsoft.VisualStudio.Code.Manifest" Path="extension/package.json" Addressable="true"/>
  </Assets>
</PackageManifest>
XML

rm -f "$OUT"
# The names go in on stdin (`-@`) because `zip` globs its arguments and `[Content_Types].xml`
# is a bracket expression to it, not a filename. `extension` recurses, so it brings
# `extension/LICENSE` with it; `LICENSE` is the root copy and has to be named on its own.
NAMES="[Content_Types].xml
extension.vsixmanifest
extension"
if [ -f "$STAGE/LICENSE" ]; then
  NAMES="$NAMES
LICENSE"
fi
( cd "$STAGE" && printf '%s\n' "$NAMES" | zip -q -r -X "$OUT" -@ )
unzip -l "$OUT" | grep -q 'Content_Types' || { echo "pack.sh: [Content_Types].xml is missing from $OUT" >&2; exit 1; }
echo "$OUT"
