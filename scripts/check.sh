#!/usr/bin/env bash
# コミット前に通すべき検査をまとめて走らせる。1 つでも落ちたら止まる。
# pre-commit フック (.githooks/pre-commit) からも呼ばれる。
set -euo pipefail
cd "$(dirname "$0")/.."

step() { printf '\n== %s\n' "$1"; }

step "TypeScript の型検査"
npx tsc --noEmit

step "Rust の整形"
(cd src-tauri && cargo fmt --check)

step "Rust の lint (警告もエラーにする)"
(cd src-tauri && cargo clippy --all-targets --quiet -- -D warnings)

step "Rust のテスト"
(cd src-tauri && cargo test --quiet)

printf '\nすべて通った\n'
