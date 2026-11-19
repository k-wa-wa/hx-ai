# AGENTS.md

このリポジトリを変更する人 (エージェントを含む) への約束。作りの説明は [README.md](README.md) にある。

## 品質はテストで守る

コメントで「こうしてはいけない」と書くより、破ったら落ちるテストを書く。

- 変更したら `npm run check` を通す。pre-commit フック (`git config core.hooksPath .githooks`) が同じものを走らせ、落ちたらコミットできない。
- 振る舞いを変える・足すときは、先に「誤った実装なら落ちるテスト」を書く。テストを書いた後に実装をわざと壊し、落ちることを一度確かめる。
- 期待値をテスト対象の関数自身で作らない (例: `assert_eq!(x, b64(..))` ではなく固定値)。

## どこにテストを書くか

| 守りたいもの | 置き場所 |
| --- | --- |
| 自動操作のガードレール (最前面・ウィンドウの移動・重なり・マウスの介入) | `src-tauri/src/automation.rs` の `FakeDesktop` を使うテスト。ガード 1 つにつきテスト 1 つ |
| サイクルと Undo の流れ (撮り直し・回数の上限・途中失敗・戻せなかった値) | `src-tauri/src/workflow_test.rs` の `FakeEngine` |
| AI API の HTTP (再試行・エラー) | `src-tauri/src/ai_http_test.rs` (wiremock) |
| AI に渡すスキーマ・プロンプトと Rust の型の一致 | `src-tauri/src/ai.rs` のテスト |
| 座標変換と境界 | `src-tauri/src/geometry.rs` のテスト |
| 画面の振る舞い | `src/App.test.tsx` (`src/test/backend.ts` の偽のバックエンド) |
| フロントエンドと Rust のコマンド名・引数名 | `src/contract.test.ts` (ソースを読んで突き合わせる) |

OS に触る処理 (`macos.rs`・`capture.rs`・`audio::record` など) は薄く保ち、判断は純粋関数に切り出してテストする。

## 変異テスト

テストが誤りに気づけるかは `cargo mutants` で測る (実装をわざと壊し、テストが落ちるかを見る)。

```sh
cargo install cargo-mutants --locked
cd src-tauri && cargo mutants -j 4                    # すべて (20 分ほど)
cd src-tauri && cargo mutants --in-diff <(git diff main)  # 変えた所だけ
```

見逃し (`mutants.out/missed.txt`) が出たら、テストを足すか、その変異が意味を持たない理由 (重複した検査など) を確かめてコードを直す。OS に触る部分は `src-tauri/.cargo/mutants.toml` で対象から外しているので、ロジックをそこへ入れない。

## AI の結合テスト

実際の API を呼ぶテスト (`cargo test ai_live -- --ignored`) は課金が発生するので、必要なときだけ走らせる。プロンプトやスキーマを変えたときは走らせる。
