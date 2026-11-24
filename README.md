# HX AI

HX Stomp / HX Stomp XL の音を聴き、USB 経由で現在の編集バッファ (プリセットのチェーンとパラメータ値) を読み取り、目標の音に近づくようにパラメータを自動で書き込む macOS 用のコンパニオンアプリである。要件は [memo.md](memo.md) にある。

本体メモリへの保存 (`save`) は行わず、一時編集バッファだけを更新する。気に入った設定を残すときは実機側で保存する。

Tauri v2 (Rust) + React + TypeScript で作り、AI は OpenAI 互換の Chat Completions API で呼ぶ (既定は Gemini の OpenAI 互換エンドポイントと `gemini-3.8-flash`)。

## 使い方

1. `/Applications/Line6/HX Edit.app` をインストールしておく (起動する必要はない。モデル定義の参照に使う)。`HX AI` が USB セッションを保持している間は HX Edit を閉じておく (USB の排他接続のため)。HX Edit を使いたいときは画面上部の「USB 解放」を押す。
2. `npm install` してから `npm run tauri dev` で起動する。
3. 右上の ⚙︎ で設定ウィンドウを開き、API キーを入れて保存する。
4. マイクの権限は、使うときに足りなければ許可を求める (ダイアログかシステム設定が開く)。許可してから、もう一度実行する。前もって許可するときは、設定ウィンドウの「macOS の権限」の「許可する」を押す。
5. 手動モードか自動ループを選ぶ (下)。
6. 結果が気に入らなければ「戻す」を押す。1 回の送信 (自動ループでは 1 回の調整) ごとに、変更前の値へ直接戻せる。

目標の音 (WAV / MP3) をドロップすると、リファレンスとして AI に渡す。AI に送るのは 20 秒の区間だけで、プレビューを再生して「再生位置から使う」を押すと区間を選び直せる。

### 手動モード

「録音」で録音を始め、ギターを弾いてからもう一度押して止める (最大 30 秒)。前後の無音を削った録音が入力欄の上に添付される。指示を書いて「送信」(⌘↩) すると、録音と指示を AI に渡して調整する。録音なしで指示だけを送ることもできる (例:「ディレイを足して」)。

録音は送ったら使い切る。AI の混雑などで何も変えずに失敗したときは、録音と指示が残るので、そのまま送り直せる。

### 自動ループ

ルーパーなどで音を出し続け、目標を書いて「自動ループを始める」を押す。録音 (3 / 5 / 10 秒) → 調整 を繰り返し、次のどれかで止まる。

- AI が目標に届いたと判断して、値を変えなかった (値を変えなかったが目標に届いていないときも止まる)
- 回数の上限 (5 / 10 / 20 回)
- 「ループを止める」。AI に問い合わせ中に押すと、その答えは実行しない
- 3 回続けて失敗した (無音・USB 通信エラー・AI の混雑など。1〜2 回なら次の回へ進む)

ループ中に入力欄から送った指示は、次の回から目標に加える。

## 設定

設定画面の値 → 既定値 の順に使う。API キーは設定画面で入れないと使えない。設定画面の値は `~/Library/Application Support/com.kwawa.hxai/settings.json` (権限 600) に保存する。

開発時 (debug ビルド。`npm run tauri dev`) だけは、設定画面で空の項目を `.env` / 環境変数で補う。配布する `.app` (release ビルド) は `.env` も環境変数も読まない。

`.env` には Gemini と OpenRouter の 2 つのブロック (それぞれ `OPENAI_API_KEY`・`OPENAI_BASE_URL`・`OPENAI_MODEL`) を書いておき、使わない方をコメントアウトして切り替える。切り替えたら開発起動をし直す。設定画面に値を保存してあると、そちらが優先される。

```sh
# Gemini
OPENAI_API_KEY=...
OPENAI_BASE_URL=https://generativelanguage.googleapis.com/v1beta/openai/
OPENAI_MODEL=gemini-3.8-flash

# OpenRouter
# OPENAI_API_KEY=...
# OPENAI_BASE_URL=https://openrouter.ai/api/v1
# OPENAI_MODEL=google/gemini-3.8-flash
```

| 項目 | 開発時の環境変数 | 既定値 |
| --- | --- | --- |
| API キー | `OPENAI_API_KEY` | なし (無ければエラー) |
| base URL | `OPENAI_BASE_URL` | `https://generativelanguage.googleapis.com/v1beta/openai/` |
| モデル | `OPENAI_MODEL` | `gemini-3.8-flash` |
## 1 サイクルの流れ

1. 録音の前にマイク権限を確かめる。
2. 録音を整える。先頭 2 チャンネル (HX Stomp の USB 1/2) を平均してモノラルにし、前後の無音を削り、16kHz の 16bit WAV にする。ピークが -50 dBFS 未満なら無音としてエラーにする。
3. HX Stomp / HX Stomp XL に USB 接続し、現在の編集バッファを読み取ってチェーンと各パラメータの値・範囲・選択肢・ロック状態 (コントローラやスナップショット割り当て中か) をテキスト化する。1 度開いた USB セッションはアプリ内で保持し、2 回目以降の読み書きを高速化する。
4. リファレンスの区間・録音・チェーンテキスト・指示を AI に送り、変更を受け取る。AI はエフェクト一覧を検索してから (下)、答えを関数 `submit_changes` で返す (`block`・`parameter`・`value`)。
5. 実行の前に、すべての変更をまとめて検証する。1 つでも外れたら何もしない。
   - 変更は 12 個まで、現在のプリセットに存在するブロック名とパラメータ名であること
   - コントローラやスナップショットでロック中のパラメータでないこと
   - 値と型がパラメータ定義に合っていること (連続値なら範囲内、選択肢なら一覧にある名前、スイッチなら On/Off)
   - プリセットの切り替え・保存・削除・モデル変更・グローバル設定などはツールやコマンドとして AI に公開しない
6. USB 経由で 1 件ずつ `set_param` で一時編集バッファへ書き込む。途中でエラーが起きた場合は、それまでに書き込んだ変更だけを Undo 履歴に積んで返す。

## エフェクト一覧

AI は関数 `search_effects` で、HX Stomp のエフェクト一覧をカテゴリ (`drive`・`amp`・`delay` など) とキーワード (「BB」「TS」「Klon」「Marshall」「ファズ」など) で検索できる。別のモデルを勧めるときや、現在のモデルが何を元にしたものか確かめるときに使う。モデルの切り替えはアプリでは実行せず、勧めたモデル名を説明に書く。

- 一覧は公式マニュアル (HX Stomp 3.80 Owner's Manual) のモデル表から作った `src-tauri/catalog/hx_stomp.json` (428 モデル)
- 系統のタグ (TS系・Klon系・BB系・Marshall系など) は、元にした機種から規則で付けた分類で、マニュアルの記載ではない。「BB系」は Bluesbreaker 系 (King of Tone・Prince of Tone) に付けてある
- 1 回の検索で返すのは 15 件までで、全件数も返す。1 回の問い合わせで検索は 3 回まで (超えたら答えを強制する)

## AI に送るデータを抑える仕組み

会話やループが続いても、1 回のリクエストが大きくならないようにしている。

| 送るもの | 抑え方 |
| --- | --- |
| 録音 | 16kHz モノラル (Gemini は 16kHz に落として聴くので、それ以上は無駄になる)。前後の無音を削る。最大 30 秒 |
| リファレンス | 選んだ 20 秒の区間だけを 16kHz モノラルで送る |
| 現在のチェーン | 接続中のプリセットの各ブロック・パラメータ名・現在値・範囲・ロック状態だけをテキストで送る |
| 会話の履歴 | 直近 8 発言、1 発言 400 文字まで。自動ループでは長い指示を繰り返さず「何回目か」だけを残す |
| これまでの変更 | 履歴とは別に、このセッションで書き込んだ値をパラメータごとに 1 行で渡す (最大 24 個) |

途中で止まった場合も、そこまでに実行した変更は「戻す」で戻せる。

## Undo

変更を実行するたびに、変更前のパラメータのネイティブ値とブロック・パラメータ位置を Undo 履歴 (最大 20 回分) に控えておく。「戻す」を押すと AI を呼ばずに USB 経由で変更前の値を直接書き戻す。途中で書き戻しに失敗した項目があっても残りを続け、戻せなかった項目を画面に表示する。

## 開発

```sh
npm install
git config core.hooksPath .githooks    # コミット前に npm run check を走らせる (clone ごとに 1 回)
npm run check                          # 型検査・整形・lint・テストをまとめて走らせる
npm run tauri dev                      # 開発起動
npm run build                          # フロントエンドの型検査とビルド
(cd src-tauri && cargo test)           # 単体テスト
(cd src-tauri && cargo clippy --all-targets)
npm run tauri build -- --bundles app   # .app を作る
```

テストの置き場所と変異テスト (cargo mutants) の使い方は [AGENTS.md](AGENTS.md) にある。

### AI の結合テスト

実際の API を呼ぶ (課金が発生する) ので既定では走らない。

```sh
cd src-tauri && cargo test ai_live -- --ignored --nocapture --test-threads=1
```

入力は [src-tauri/src/ai_live_test.rs](src-tauri/src/ai_live_test.rs) に埋め込んだチェーンテキスト (`SAMPLE_CHAIN`) と合成のこぎり波 (16kHz モノラル WAV) である。Treble を下げられるか、Delay Mix を直接指定値に変えられるか、一覧から Bluesbreaker 系のモデルを勧められるかを確かめる。

### 実機 USB テスト

実機を接続したときだけ手動で走らせる (既定では走らない)。安全のため、スロット末尾が `*A` のプリセット上では中断する。

```sh
cd src-tauri && HX_USB_LIVE=1 cargo test --lib hx_usb_round_trip -- --ignored --nocapture
```

### ソースの構成

| ファイル | 役割 |
| --- | --- |
| `src-tauri/src/workflow.rs` | 1 サイクル・Undo・手動送信・自動ループの流れ、Undo 履歴 |
| `src-tauri/src/ai.rs` | リクエストの組み立て、応答の解釈と検証、API 呼び出し (429/5xx は 2 回まで再試行) |
| `src-tauri/src/hx_device.rs` | USB 通信 (`hx-usb`・`hx-proto`・`hx-catalog`) のラップ、チェーン文字列化、変更の検証と実行、Undo 復元 |
| `src-tauri/src/catalog.rs` | エフェクト一覧 (`catalog/hx_stomp.json`) の検索 |
| `src-tauri/src/audio.rs` | cpal による録音 (開始/停止できる録音器と、秒数を決めた録音) |
| `src-tauri/src/sound.rs` | 録音とリファレンスを AI に送る形に整える (モノラル化・無音の削除・リサンプル・MP3 のデコード) |
| `src-tauri/src/macos.rs` | マイク権限の確認・要求 |
| `src-tauri/src/permission.rs` | 足りない権限と「許可する」で何をするかの判断 (純粋関数) |
| `src-tauri/src/config.rs` | 設定の保存と解決 |
| `src/App.tsx` ほか | 画面 |
