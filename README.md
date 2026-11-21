# HX AI

マルチエフェクターの音を聴き、編集アプリの画面を読んで、目標の音に近づくようにパラメータを自動で変える macOS 用のコンパニオンアプリである。要件は [memo.md](memo.md) にある。

| 機種 | 編集アプリ | 値の変え方 |
| --- | --- | --- |
| Line 6 HX Stomp | HX Edit | 数値欄をダブルクリックして値を打ち込む |
| Valeton GP-5 | Valeton Suite (iPad 版を Apple Silicon の Mac で動かす。GP-5 とは Bluetooth で繋ぐ) | スライダーをドラッグする |

機種は設定ウィンドウで選ぶ。録音はどちらも USB オーディオで行う (GP-5 も USB でオーディオインターフェースになる)。

Tauri v2 (Rust) + React + TypeScript で作り、AI は OpenAI 互換の Chat Completions API で呼ぶ (既定は Gemini の OpenAI 互換エンドポイントと `gemini-3.8-flash`)。

## 使い方

1. `npm install` してから `npm run tauri dev` で起動する。
2. 右上の ⚙︎ で設定ウィンドウを開き、API キーを入れて保存する。
3. マイク・画面収録・アクセシビリティの権限は、使うときに足りなければ許可を求める (ダイアログかシステム設定が開く)。許可してから、もう一度実行する。前もって許可するときは、設定ウィンドウの「macOS の権限」の「許可する」を押す。
4. 設定ウィンドウで機種を選び、編集アプリ (HX Edit / Valeton Suite) を開いて、このアプリと重ならないように横に並べる。
5. 手動モードか自動ループを選ぶ (下)。
6. 結果が気に入らなければ「戻す」を押す。1 回の送信 (自動ループでは 1 回の調整) ごとに戻せる。

目標の音 (WAV / MP3) をドロップすると、リファレンスとして AI に渡す。AI に送るのは 20 秒の区間だけで、プレビューを再生して「再生位置から使う」を押すと区間を選び直せる。

### 手動モード

「録音」で録音を始め、ギターを弾いてからもう一度押して止める (最大 30 秒)。前後の無音を削った録音が入力欄の上に添付される。指示を書いて「送信」(⌘↩) すると、録音と指示を AI に渡して調整する。録音なしで指示だけを送ることもできる (例:「ディレイを足して」)。

録音は送ったら使い切る。AI の混雑などで何も変えずに失敗したときは、録音と指示が残るので、そのまま送り直せる。

### 自動ループ

ルーパーなどで音を出し続け、目標を書いて「自動ループを始める」を押す。録音 (3 / 5 / 10 秒) → 調整 を繰り返し、次のどれかで止まる。

- AI が目標に届いたと判断して、値を変えなかった (値を変えなかったが目標に届いていないときも止まる)
- 回数の上限 (5 / 10 / 20 回)
- 「ループを止める」。AI に問い合わせ中に押すと、その答えは実行しない
- 自動操作のガードレールで止まった (マウスに触った・別のアプリが前に出たなど)
- 3 回続けて失敗した (無音・AI の混雑など。1〜2 回なら次の回へ進む)

ループ中に入力欄から送った指示は、次の回から目標に加える。

## 設定

設定画面の値 → 既定値 の順に使う。API キーは設定画面で入れないと使えない。設定画面の値は `~/Library/Application Support/com.kwawa.hxai/settings.json` (権限 600) に保存する。

開発時 (debug ビルド。`npm run tauri dev`) だけは、設定画面で空の項目を `.env` / 環境変数で補う。配布する `.app` (release ビルド) は `.env` も環境変数も読まない。

| 項目 | 開発時の環境変数 | 既定値 |
| --- | --- | --- |
| API キー | `OPENAI_API_KEY` | なし (無ければエラー) |
| base URL | `OPENAI_BASE_URL` | `https://generativelanguage.googleapis.com/v1beta/openai/` |
| モデル | `OPENAI_MODEL` | `gemini-3.8-flash` |
| 機種 | なし | HX Stomp |
| 操作するアプリ名 | `HX_TARGET_APP` | 機種で決まる (`HX Edit` / `Valeton Suite`) |

## 1 サイクルの流れ

1. 権限を確かめる (自動ループは編集アプリのウィンドウがあることも、録音の前に確かめる)。
2. 録音を整える。先頭 2 チャンネル (HX Stomp の USB 1/2) を平均してモノラルにし、前後の無音を削り、16kHz の 16bit WAV にする。ピークが -50 dBFS 未満なら無音としてエラーにする。
3. 編集アプリのウィンドウだけをキャプチャし、その時のウィンドウの位置と大きさを控える。長辺 1600px に縮める。
4. リファレンスの区間・録音・スクショ・指示を AI に送り、操作を受け取る。AI はエフェクト一覧を検索してから (下)、答えを関数 `submit_actions` で返す。座標はスクショ上の 0〜1000 の正規化座標で受け取る。
5. 実行の前に、すべての操作をまとめて検証する。1 つでも外れたら何もしない。
   - 値は単位なしの数値だけ、変更は 12 個まで、座標は範囲内
   - GP-5 はスライダーの範囲 (min / max) があり、値がその中にある
   - 座標がウィンドウの中で、タイトルバーと縁にかかっていない
6. 編集アプリを最前面にし、操作ごとに次を確かめてから値を変える。
   - 最前面のアプリが編集アプリである
   - キャプチャした時からウィンドウが動いていない
   - その座標で一番手前のウィンドウが編集アプリである (このアプリなどが重なっていない)
   - 前の操作からマウスが動かされていない

   HX Stomp は数値欄をダブルクリック → 値を入力 → Enter する。GP-5 は次のようにスライダーを合わせる。
   - ウィンドウを撮り、指した点のスライダーの両端と青い塗りの端を画素で読む
   - 塗りの位置が、AI の言う変更前の値と範囲から計算した位置と 3% 以上ずれていたら、別のスライダーを指したか範囲を読み違えたとみなして何もしない
   - 塗りの端から目標の位置まで細かく分けてドラッグし、撮り直して確かめる。ずれていれば 3 回まで直す (前のドラッグで足りなかった分は、次に多めに動かす)。合わなければ止める
7. AI が画面に出ていないブロックを選ぶ (`select_block`) と返したら、クリックして撮り直し、もう一度聞く (最大 4 回)。

## エフェクト一覧

AI は関数 `search_effects` で、その機種のエフェクト一覧をカテゴリ (`drive`・`amp`・`delay` など) とキーワード (「BB」「TS」「Klon」「Marshall」「ファズ」など) で検索できる。別のモデルを勧めるときや、画面のモデルが何を元にしたものか確かめるときに使う。モデルの切り替えはアプリでは実行せず、勧めたモデル名を説明に書く。

- 一覧は各機種の公式マニュアルのモデル表から作った `src-tauri/catalog/hx_stomp.json` (HX Stomp 3.80 Owner's Manual、428 モデル) と `gp5.json` (GP-5 User Manual V1.0.3、111 モデル)。GP-5 にはパラメータ名も入れてある
- 系統のタグ (TS系・Klon系・BB系・Marshall系など) は、元にした機種から規則で付けた分類で、マニュアルの記載ではない。「BB系」は Xotic BB Preamp 系と Bluesbreaker 系の両方に付けてあり、どちらかは別のタグで分かる
- 1 回の検索で返すのは 15 件までで、全件数も返す。1 回の問い合わせで検索は 3 回まで (超えたら答えを強制する)

## AI に送るデータを抑える仕組み

会話やループが続いても、1 回のリクエストが大きくならないようにしている。

| 送るもの | 抑え方 |
| --- | --- |
| 録音 | 16kHz モノラル (Gemini は 16kHz に落として聴くので、それ以上は無駄になる)。前後の無音を削る。最大 30 秒 |
| リファレンス | 選んだ 20 秒の区間だけを 16kHz モノラルで送る |
| スクリーンショット | 長辺 1600px |
| ブロックを選び直した後の撮影 | 音声は送らず、前の撮影での AI の説明で方針を伝える |
| 会話の履歴 | 直近 8 発言、1 発言 400 文字まで。自動ループでは長い指示を繰り返さず「何回目か」だけを残す |
| これまでの変更 | 履歴とは別に、このセッションで打ち込んだ値をパラメータごとに 1 行で渡す (最大 24 個) |

途中で止まった場合も、そこまでに実行した変更は「戻す」で戻せる。

## Undo

AI がスクショから読んだ変更前の値を控えておき、「戻す」でその値を打ち直す。打つ場所は、その時の画面を撮り直して AI に出させる (ユーザーが別のブロックを選んでいても、違うパラメータを上書きしないため)。AI が控えた値以外を返したら実行しない。戻しきれなかった値は残り、もう一度「戻す」を押せる。

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

入力は模擬画面である。

- HX Edit 風 ([scripts/make_mock_screenshot.swift](scripts/make_mock_screenshot.swift) → `src-tauri/tests/fixtures/mock_hx_edit.png`): Treble の数値欄・Delay ブロックの座標を正しく指せるか、変更前の値を読めるか、一覧から Bluesbreaker 系のモデルを勧められるか
- Valeton Suite 風 ([scripts/make_mock_valeton.swift](scripts/make_mock_valeton.swift) → `src-tauri/tests/fixtures/mock_valeton.png`): Treble のスライダーを指し、範囲と変更前の値を返し、その点の画素がアプリの安全確認を通るか。スライダーの画素読み取りの単体テストにも使う

描き直すときは次を実行する。

```sh
swift scripts/make_mock_screenshot.swift src-tauri/tests/fixtures/mock_hx_edit.png
swift scripts/make_mock_valeton.swift src-tauri/tests/fixtures/mock_valeton.png
```

### ソースの構成

| ファイル | 役割 |
| --- | --- |
| `src-tauri/src/workflow.rs` | 1 サイクル・Undo・手動送信・自動ループの流れ、Undo 履歴 |
| `src-tauri/src/ai.rs` | リクエストの組み立て、応答の解釈と検証、API 呼び出し (429/5xx は 2 回まで再試行) |
| `src-tauri/src/automation.rs` | enigo による操作 (HX は打ち込み、GP-5 はドラッグ) と、操作ごとのガードレール |
| `src-tauri/src/slider.rs` | Valeton Suite のスライダーを画素から読む (純粋関数) |
| `src-tauri/src/catalog.rs` | エフェクト一覧 (`catalog/*.json`) の検索 |
| `src-tauri/src/device.rs` | 機種と、機種ごとの既定の編集アプリ |
| `src-tauri/src/geometry.rs` | 座標変換とガードレールの判定 (純粋関数) |
| `src-tauri/src/capture.rs` | xcap によるウィンドウの検出とキャプチャ |
| `src-tauri/src/audio.rs` | cpal による録音 (開始/停止できる録音器と、秒数を決めた録音) |
| `src-tauri/src/sound.rs` | 録音とリファレンスを AI に送る形に整える (モノラル化・無音の削除・リサンプル・MP3 のデコード) |
| `src-tauri/src/macos.rs` | 権限の確認・要求、最前面アプリの判定とアクティブ化 |
| `src-tauri/src/permission.rs` | 足りない権限と「許可する」で何をするかの判断 (純粋関数) |
| `src-tauri/src/config.rs` | 設定の保存と解決 |
| `src/App.tsx` ほか | 画面 |
