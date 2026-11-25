#!/usr/bin/env bash
# 時間のかかる検査 (release ビルド・変異テスト) をまとめて走らせる。
# AI 呼び出しテスト (ai_live) は課金と外部依存を避けるため含めない。
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$1"; }

./scripts/check.sh

step "フロントエンドの本番ビルド"
npm run build

step "Tauri アプリの本番ビルド (.app)"
npm run tauri build -- --bundles app

if ! command -v cargo-mutants >/dev/null 2>&1; then
  printf '\n[ERROR] cargo-mutants が見つかりません。`cargo install cargo-mutants --locked` で入れてください\n' >&2
  exit 1
fi

step "変異テスト (cargo mutants)"
(cd src-tauri && cargo mutants -j 4)

printf '\nNightly チェックがすべて通った\n'
