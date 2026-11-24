#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

BUILD_TYPE=$1

if [[ "$BUILD_TYPE" == "build" ]]; then
MJS_FILE="index.mjs"
BINARY_FILE="rust-liquid.linux-x64-gnu.node"

npx napi build --platform --js index.cjs --config-path napi.config.json

cat << 'EOF' > "$MJS_FILE"
// Generated form ci.yml
import { createRequire } from "module";
const require = createRequire(import.meta.url);
const nativeBinding = require("./index.cjs");

export const LiquidEngine = nativeBinding.LiquidEngine;
export const frontMatter = nativeBinding.frontMatter
export const LiquidExtension = nativeBinding.LiquidExtension
export const SourcePriority = nativeBinding.SourcePriority
export const splitFrontMatter = nativeBinding.splitFrontMatter
EOF

npx oxlint index.cjs
npx oxlint index.mjs

npx oxfmt --write index.cjs
npx oxfmt --write index.mjs
npx oxfmt --write index.d.ts

rm -rf "$BINARY_FILE"

sleep 3

node -e "
const fs = require('fs');
const pkg = require('./package.json');
try {
  pkg.gitHead = require('child_process').execSync('git rev-parse HEAD').toString().trim();
  fs.writeFileSync('./package.json', JSON.stringify(pkg, null, 2));
  console.log('Successfully added gitHead:', pkg.gitHead);
} catch (e) {
  console.error('Failed to read git HEAD or write package.json', e);
}"

npx oxfmt --write package.json

echo "Finished Build"
fi


if [[ "$BUILD_TYPE" == "local" ]]; then

OUT_DIR="test/liquid"
MJS_FILE="$OUT_DIR/index.mjs"
PACKAGE_FILE="$OUT_DIR/package.json"

npx napi build --platform --js index.cjs --config-path napi.config.json --output-dir "$OUT_DIR"

cat << 'EOF' > "$MJS_FILE"
// Generated form ci.yml
import { createRequire } from "module";
const require = createRequire(import.meta.url);
const nativeBinding = require("./index.cjs");

export const LiquidEngine = nativeBinding.LiquidEngine;
export const frontMatter = nativeBinding.frontMatter
export const LiquidExtension = nativeBinding.LiquidExtension
export const SourcePriority = nativeBinding.SourcePriority
export const splitFrontMatter = nativeBinding.splitFrontMatter
EOF

cat << 'EOF' > "$PACKAGE_FILE"
{
 "name": "liquid",
 "main": "index.cjs",
 "types": "index.d.ts",
 "module": "index.mjs"
}
EOF

echo "Finished Build"

fi





