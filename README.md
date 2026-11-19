# HX AI

HX Stomp の音を聴き、HX Edit の画面を読んで、目標の音に近づくようにパラメータを自動で打ち込む macOS 用のコンパニオンアプリである。要件は [memo.md](memo.md) にある。

Tauri v2 (Rust) + React + TypeScript で作り、AI は OpenAI 互換の Chat Completions API で呼ぶ (既定は Gemini の OpenAI 互換エンドポイントと `gemini-3.8-flash`)。

## 使い方

1. `.env` に `OPENAI_API_KEY` を書く (設定画面で入力してもよい)。
2. `npm install` してから `npm run tauri dev` で起動する。
3. 右上の権限表示 (3 つの点) から、マイク・画面収録・アクセシビリティを許可する。
4. HX Edit を開き、このアプリと重ならないように横に並べる。
5. 指示を書いて「聴いて調整する」(または ⌘⌥H) を押し、録音中にギターを弾く。
6. 結果が気に入らなければ「戻す」を押す。

## 設定

設定画面の値 → `.env` / 環境変数 → 既定値 の順に使う。`.env` は起動したディレクトリから上にたどって探すので、リポジトリで `npm run tauri dev` したときだけ効く。Finder から `.app` を開くときは設定画面でキーを入れる。設定画面の値は `~/Library/Application Support/com.kwawa.hxai/settings.json` (権限 600) に保存する。

| 項目 | 環境変数 | 既定値 |
| --- | --- | --- |
| API キー | `OPENAI_API_KEY` | なし (無ければエラー) |
| base URL | `OPENAI_BASE_URL` | `https://generativelanguage.googleapis.com/v1beta/openai/` |
| モデル | `OPENAI_MODEL` | `gemini-3.8-flash` |
| 操作するアプリ名 | `HX_TARGET_APP` | `HX Edit` |

## 1 サイクルの流れ

1. 権限と HX Edit のウィンドウがあることを確かめる (弾き終わってから失敗させないため)。
2. 入力デバイスから 3 秒か 5 秒録音する。先頭 2 チャンネル (HX Stomp の USB 1/2) を 16bit WAV にする。ピークが -50 dBFS 未満なら無音としてエラーにする。
3. HX Edit のウィンドウだけをキャプチャし、その時のウィンドウの位置と大きさを控える。
4. リファレンス音源・録音・スクショ・指示を AI に送り、JSON で操作を受け取る。座標はスクショ上の 0〜1000 の正規化座標で受け取る。
5. 実行の前に、すべての操作をまとめて検証する。1 つでも外れたら何もしない。
   - 値は単位なしの数値だけ、変更は 12 個まで、座標は範囲内
   - 座標がウィンドウの中で、タイトルバーと縁にかかっていない
6. HX Edit を最前面にし、操作ごとに次を確かめてから、数値欄をダブルクリック → 値を入力 → Enter する。
   - 最前面のアプリが HX Edit である
   - キャプチャした時からウィンドウが動いていない
   - その座標で一番手前のウィンドウが HX Edit である (このアプリなどが重なっていない)
   - 前の操作からマウスが動かされていない
7. AI が画面に出ていないブロックを選ぶ (`select_block`) と返したら、クリックして撮り直し、もう一度聞く (最大 4 回)。

途中で止まった場合も、そこまでに実行した変更は「戻す」で戻せる。

## Undo

AI がスクショから読んだ変更前の値を控えておき、「戻す」でその値を打ち直す。打つ場所は、その時の画面を撮り直して AI に出させる (ユーザーが別のブロックを選んでいても、違うパラメータを上書きしないため)。AI が控えた値以外を返したら実行しない。戻しきれなかった値は残り、もう一度「戻す」を押せる。

## 開発

```sh
npm install
npm run tauri dev                      # 開発起動
npm run build                          # フロントエンドの型検査とビルド
(cd src-tauri && cargo test)           # 単体テスト
(cd src-tauri && cargo clippy --all-targets)
npm run tauri build -- --bundles app   # .app を作る
```

### AI の結合テスト

実際の API を呼ぶ (課金が発生する) ので既定では走らない。

```sh
cd src-tauri && cargo test ai_live -- --ignored --nocapture --test-threads=1
```

入力は [scripts/make_mock_screenshot.swift](scripts/make_mock_screenshot.swift) で描いた HX Edit 風の模擬画面 (`src-tauri/tests/fixtures/mock_hx_edit.png`) である。Treble の数値欄・Delay ブロックの座標を正しく指せるか、変更前の値を読めるかを確かめる。描き直すときは次を実行する。

```sh
swift scripts/make_mock_screenshot.swift src-tauri/tests/fixtures/mock_hx_edit.png
```

### ソースの構成

| ファイル | 役割 |
| --- | --- |
| `src-tauri/src/workflow.rs` | 1 サイクルと Undo の流れ、Undo 履歴 |
| `src-tauri/src/ai.rs` | リクエストの組み立て、応答の解釈と検証、API 呼び出し (429/5xx は 2 回まで再試行) |
| `src-tauri/src/automation.rs` | enigo による操作と、操作ごとのガードレール |
| `src-tauri/src/geometry.rs` | 座標変換とガードレールの判定 (純粋関数) |
| `src-tauri/src/capture.rs` | xcap によるウィンドウの検出とキャプチャ |
| `src-tauri/src/audio.rs` | cpal による録音と WAV 化 |
| `src-tauri/src/macos.rs` | 権限の確認・要求、最前面アプリの判定とアクティブ化 |
| `src-tauri/src/config.rs` | 設定の保存と解決 |
| `src/App.tsx` ほか | 画面 |
