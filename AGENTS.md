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
| 数値の行の検出 (上下の矢印・数値の文字・矢印の無い行を数えない) | `src-tauri/src/value_fields.rs` のテスト。実物の HX Edit の画面 (`tests/fixtures/real_hx_*.png`) で確かめる。新しい種類の画面で誤ったら、その画面を足す |
| 行の番号の照合と、押す位置 | `src-tauri/src/automation.rs` のテスト (`FakeDesktop` は 50 ピクセルごとに数値の行を描いた画面を持つ) |
| ダブルクリックの前後の撮り比べ | `src-tauri/src/screen_check.rs` (判定) と `automation.rs` (ダイアログが開いたときの Escape) |
| エフェクト一覧の中身と検索 | `src-tauri/src/catalog.rs` のテスト。件数を固定してあるので、一覧を作り直したら件数も直す |
| ツール呼び出し (検索 → 結果 → 答え、検索の上限) | `src-tauri/src/ai.rs` のテスト (返事の解釈) と `src-tauri/src/ai_http_test.rs` (往復) |
| サイクルと Undo の流れ (撮り直し・回数の上限・途中失敗・戻せなかった値) | `src-tauri/src/workflow_test.rs` の `FakeEngine` |
| 手動送信と自動ループの流れ (録音の使い切り・止まる条件・停止の合図・途中の指示) | `src-tauri/src/workflow_test.rs` の `FakeEngine` (録音とフックも台本で与える) |
| 録音とリファレンスの加工 (無音の削除・リサンプル・区間の切り出し) | `src-tauri/src/sound.rs` のテスト。リサンプルは理想の正弦波と 1 サンプルずつ比べる |
| AI API の HTTP (再試行・エラー) | `src-tauri/src/ai_http_test.rs` (wiremock) |
| AI に渡すスキーマ・プロンプトと Rust の型の一致 | `src-tauri/src/ai.rs` のテスト |
| 座標変換と境界 | `src-tauri/src/geometry.rs` のテスト |
| 画面の振る舞い | `src/App.test.tsx` (`src/test/backend.ts` の偽のバックエンド) |
| フロントエンドと Rust のコマンド名・引数名 | `src/contract.test.ts` (ソースを読んで突き合わせる) |

OS に触る処理 (`macos.rs`・`capture.rs`・`audio.rs` など) は薄く保ち、判断は純粋関数に切り出してテストする。

## 変異テスト

テストが誤りに気づけるかは `cargo mutants` で測る (実装をわざと壊し、テストが落ちるかを見る)。

```sh
cargo install cargo-mutants --locked
cd src-tauri && cargo mutants -j 4                    # すべて (20 分ほど)
cd src-tauri && cargo mutants -j 4 --in-diff <(git diff --relative <基準のコミット> -- .)  # 変えた所だけ
```

`--in-diff` の差分は src-tauri から見た相対パスにする (`--relative` が無いと `src-tauri/src/...` になり、変異が 1 つも選ばれない)。

見逃し (`mutants.out/missed.txt`) が出たら、テストを足すか、その変異が意味を持たない理由 (重複した検査など) を確かめてコードを直す。OS に触る部分は `src-tauri/.cargo/mutants.toml` で対象から外しているので、ロジックをそこへ入れない。

## AI の結合テスト

実際の API を呼ぶテスト (`cargo test ai_live -- --ignored`) は課金が発生するので、必要なときだけ走らせる。プロンプトやスキーマを変えたときは走らせる。
