# 引き継ぎ書: 画面操作から .hlx の出し入れへの作り替え

2026-11-24 時点。このリポジトリの前提知識が無くても、これと [AGENTS.md](AGENTS.md)・[README.md](README.md) を読めば続きを始められるように書く。

## 0. まず守ること

- **人間とは日本語でやりとりする。** 途中経過・報告・表もすべて日本語。前任は英語で何度も書いてしまい、人間を強く怒らせた。
- 文書とコメントは「である調」。
- git: このリポジトリではコミットしてよい (人間が許可済み)。**push はしない** (リモートも無い)。人間の未コミットの変更 (今は `AGENTS.md` の「日本語でやりとりする。」) を勝手にコミットしない。
- 変更したら `npm run check` を通す (pre-commit フックも同じものを走らせる)。テストを先に書き、実装をわざと壊して落ちることを一度確かめる (AGENTS.md)。
- **重いテスト (`cargo mutants`) は走らせる前に人間に断る。** PC の負荷が上がり、HX Stomp の接続が切れた原因かもしれないと人間が言った。
- 実 API (AI) のテストは課金されるので回数を絞る。`.env` のキーの値は表示しない。
- 夜間 (人間が寝ている間) は、アプリの起動・録音・自動操作をしない。
- 実機 (HX Edit・HX Stomp) を自動で操作するのは、人間がその場で走らせるときだけ。このエージェントのシェルにはアクセシビリティの許可が無い (`AXIsProcessTrusted()` が false) ので、実機の確認は人間に頼む。
- 人間に実機で走らせてもらうときは、1 か所ずつ直しては頼む、を繰り返さない。前任はこれで人間の時間を大きく使った。全体の流れが成り立つかを先に確かめ、1 回の実行でできるだけ多くが分かるようにする (失敗したら画面の状態やフォルダの中身をエラーに添えるなど)。

## 1. このアプリは何か

hx-ai は macOS 用の Tauri v2 (Rust + React) アプリで、ギターのエフェクター HX Stomp XL (Line 6) の音作りを AI に手伝わせる。

- 録音した音 (とリファレンス音源・人間の指示) を AI (OpenAI 互換 API。今は OpenRouter 経由の Gemini) に送り、AI がパラメータの変更を決め、アプリが HX Edit (Line 6 の編集ソフト 3.82) に反映する。
- 使い方は 2 つ: 手動モード (録音ボタン → 指示と送る、を繰り返す) と、自動ループ (ルーパーで鳴らし続け、録音 → 変更 を自動で回す)。
- 詳しい使い方と作りは README.md。

## 2. いま何をしているか (目的)

**反映のしかたを「画面を読んで数値欄を押す」から「プリセットのファイル (.hlx) を出し入れする」に作り替えている。**

理由: HX Edit は画面の部品をアクセシビリティに出しておらず (ウィンドウの中は AXScrollArea 1 つだけ)、スクショと座標で操作するしかなかった。実機で AI が数値ではなく星マークを押す誤操作が起き、画素で数値の行を探す仕組み (value_fields.rs) まで作ったが、壊れやすい。人間が「.hlx を作って import するのはあり？」と提案し、この方式に決めた。

新しい 1 サイクル:

1. **取る**: HX Edit で今のプリセットを .hlx に書き出す (自動)
2. **聞く**: 中身 (文字) と録音を AI に渡し、「ブロック・パラメータ名・新しい値」の一覧を受け取る
3. **作る**: 変更を当てた .hlx を作る
4. **入れる**: その .hlx を HX Edit に読み込ませる (自動)
5. 録音して 2 へ。Undo は、変える前の .hlx を 4 で入れ直すだけ。

人間の要件:

- (a) ループの開始も自動 (人が手で書き出さない)
- (b) ループ中に人が手で変えた値も取り込む
- 書き出しは自動でなければならない (人間の言葉:「自動ループしたいから書き出しは自動じゃないとダメ」)
- Setlist (全プリセット) を書き出す方法は不可 (人間の言葉:「setlistは全部の情報だから書き出したくない」)
- 人間と既に合意していること: 書き出す前に毎回保存してよい (作業用スロットと割り切る)。スナップショットやコントローラーに割り当てた項目は変えない。新しい方式が実機で動いたら、画面を読む方式は外す (履歴には残る)。

## 3. どこまでできたか

| 部品 | ファイル | 状態 |
| --- | --- | --- |
| .hlx を読み、AI 向けの文にし、変更を当てて書き戻す | `src-tauri/src/preset.rs` | 完成・テスト済み。実機の .hlx で画面と同じ値になる |
| 画面の作り (アクセシビリティの木) から部品を探す | `src-tauri/src/ax_tree.rs` | 完成・テスト済み |
| 保存・書き出し・読み込みの流れ | `src-tauri/src/hx_io.rs` | 完成・偽の HX Edit (`FakeHx`) でテスト済み |
| macOS のアクセシビリティの層 (`hx_io::Ui` の本物) | `src-tauri/src/ax.rs` | 実機で書き出し 1 回・読み込み 1 回が通った |
| 実機の往復テスト | `src-tauri/src/hx_io_live_test.rs` | 書き出し → 書き換え → 読み込み までは通る。その後の書き出しで止まる (4 章) |
| 2. AI への問い合わせを .hlx 向けにする | `src-tauri/src/ai.rs` | **未着手** |
| 1〜5 をサイクル・Undo・ループに組み込む | `src-tauri/src/workflow.rs` ほか | **未着手** |
| 画面を読む古い方式を外す | `automation.rs`・`value_fields.rs`・`screen_check.rs`・`capture.rs`・`geometry.rs` ほか | **未着手** (新しい方式が実機で動いてから) |

アプリ本体はまだ古い画面方式で動く。新しい部品 (`preset.rs`・`ax_tree.rs`・`hx_io.rs`・`ax.rs`) はどこからも使われておらず、`#![allow(dead_code)]` で警告を止めている。組み込んだらこの行を消す。

関連するコミット (新しい順): `023be22` `5e6f3c2` `6e5b0d2` `4cded25` `ce1f38a` `539958f` `73eaf0b` `691ee97`。

### 3.1 preset.rs の要点

- HX Edit の中の定義を読む: `/Applications/Line6/HX Edit.app/Contents/Resources` の `*.models` (モデルごとのパラメータ: 保存名・表示名・値の型・範囲・表示の種類) と `HelixControls.json` (表示の種類ごとの倍率 `dspToDisplayScale`・書式・選択肢の表示名)。
- 例: Gain の保存値 0.22 → 表示 2.2 (`generic_knob` は ×10)。Time 0.6 → 600 ms (`time_ms` は ×1000)。Clipping 1 → "Boost" (選択肢)。
- AI に見せる文の例: `- Gain 1: 2.2 (0〜10)`、`- Clipping 1: Boost [Overdrive / Boost / Distortion]`、`- Time: 600 ms (0〜8000)`。ブロックは `dsp0.block0` のような場所で指す。
- 全パラメータ 6000 余りのうち 5910 を扱える。扱えない約 130 (Pan 101 など、表示が保存値と別に決まるもの・選択肢の数が範囲と合わないもの) は AI に見せない。
- 変更は範囲外なら丸めずエラー。1 つでも当てられなければ何も変えない。`@enabled` (ブロックの On/Off) と `@stereo` は変えない。
- スナップショットやコントローラーに割り当てた項目は `data.tone.controller.<dsp>.<block>.<param>` があれば「固定」として変えない。**この形は既知の Helix の形からの推定で、スナップショットを使った実物のプリセットではまだ確かめていない。**
- 書き戻しでキーの順を変えないよう、serde_json の `preserve_order` を使う。
- テスト用の実物の .hlx: `src-tauri/tests/fixtures/real_preset_tone_sovereign_brit.hlx` (人間の HX Stomp XL から書き出したもの)。本物の定義を読むテストは `cargo test real_hx_edit_definitions -- --ignored --nocapture` (HX Edit が入っている Mac でだけ)。
- 変異テスト (`cargo mutants -f src/preset.rs`) は負荷のため途中で止めた。未実施。走らせる前に人間に断る。

### 3.2 hx_io.rs / ax.rs の流れ

- **保存**: HX Edit を最前面にする → File > Save Preset を AXPress → 0.5 秒後にダイアログが出ていないか見る (出ていたら触らずエラー)。
- **書き出し**: 最前面にする → File > Export Preset... を AXPress → 保存の画面 (`save-panel`) が開くのを待つ → Cmd+Shift+G で「フォルダへ移動」(`GoToWindow`) を開く → `PathTextField` にフォルダのパスを AXValue で入れ、Return を押す → 「場所」(`where popup`) の値がフォルダ名と一致するか確かめる → 名前 (`saveAsNameTextField`) を入れて確かめる → `OKButton` を押す → ファイルができるのを待つ。失敗したら開いた画面を閉じる (Cancel)。
- **読み込み**: ダイアログが開いていないことを確かめる → 最前面にする → `open -b com.line6.hxedit <file>` → 確認の文に**自分のファイルのパスが書かれている**ダイアログの「Yes」だけを押す → 閉じるのを待つ → 1.5 秒待つ。
- 押す前に、写し取ったときと同じ部品か (役割・ID・題名) を確かめ、画面が変わっていたら押さない。

## 4. どこで詰まっているか

### 4.1 実機で確かめた事実

| # | 事象 | 結果 |
| --- | --- | --- |
| F1 | 保存 (Save Preset) をする | 左の一覧で今のプリセットの強調表示 (青い帯) が外れ、File > Import Preset... と Export Preset... が灰色 (押せない) になる。Import Setlist / Export Setlist は押せる |
| F2 | `open` + Yes で読み込む | 読み込みは効く (Gain 1 が 2.2 → 3.2 になった)。F1 と同じく強調表示が外れ、Export が押せなくなる |
| F3 | 一覧で別のプリセットに切り替える | 強調表示が戻り、Export が押せるようになる |
| F4 | 書き出す | 保存済みの値だけが出る。保存していない手の変更は入らない |
| F5 | HX Edit が最前面でない | Export Preset... を押しても保存の画面が開かない (人間が HX Edit に切り替えたときに開いた) |
| F6 | 所要時間 | 書き出し 4.4 秒、読み込み 2.4 秒 (確認ダイアログの後、HX Edit の読み込みに約 1 秒) |
| F7 | 画面の作りの調査の台本を走らせた後 | HX Edit が "Connection Interrupted" になり、HX Edit を開き直しても直らず、HX Stomp の再起動で直った。原因は不明 (台本は機器に何も送っていない。PC の負荷、ケーブルの可能性) |

Export/Import Preset は「一覧で強調表示されたプリセット」に対して働く (メニューの Import の確認文が "overwrite highlighted presets?" だった)。F1・F2 で強調表示が外れるのが、押せなくなる原因と見ている。

### 4.2 それで何が起きるか

| 場面 | 起きること | 要件への影響 |
| --- | --- | --- |
| ループの 2 回目以降 | 読み込みの後なので書き出せない | アプリが自分で書き換えたファイルを覚えていれば書き出しは要らないので、**問題にならない見込み** (ただし U1 次第) |
| ループ中に人が手で値を変えた | 書き出しには保存が要るが (F4)、保存すると書き出せない (F1)。取り込めないまま次を読み込むと**手の変更が消える** | **(b) を満たせない** |
| ループの開始 | 直前に保存や読み込みをしていると書き出せない | (a) が条件付き |

### 4.3 まだ分からないこと (これで次の道が決まる)

- **U1 (最重要)**: 強調表示が外れた状態 (Export が灰色) でも、`open` + Yes の読み込みは効くか。効かなければ、**ループそのものが 2 回目で止まり、Undo もできない**。
- **U2**: 一覧で**今と同じ**プリセットをクリックすると強調表示が戻り Export が押せるようになるか。そのとき、読み込み直しによる音の途切れや値の変化が起きないか。
- **U3**: F7 の接続が切れた原因。

前任は U1・U2 の手での確認を人間に頼んだところで引き継いだ。結果はまだ受け取っていない。手順:

1. U1: Export が灰色の状態で、次を実行して「Yes」を押し、Gain 1 が 2.2 に戻るかを見る (元のファイル。一時フォルダなので Mac を再起動すると消える)。
   ```sh
   open -b com.line6.hxedit /var/folders/s9/r6kbxwl16q7cczz_q9j6khxr0000gn/T/hx-ai-live-1795529257/original.hlx
   ```
2. U2: その後、一覧で 28B をクリックし、Export が押せるようになるか、音や値が変わらないかを見る。

なお同じフォルダに 23:16 の `New Preset.hlx` ができていた (人間が手で書き出したものと思われる。何を確かめたのかは未確認)。

### 4.4 結果ごとの次の道

- U1 が効く → (a) とループの 2 回目以降は成り立つ。5 章の残りの作業に進む。
- U1 が効き、U2 も「戻る・副作用なし」→ 保存や読み込みの後に、一覧の今のプリセットの行を自動でクリックし直す仕組みを足して (b) を満たす。一覧はアクセシビリティに出ていないので、行の位置は画面の色 (今のプリセットは黄色の文字) で探すことになり、画面を読む方式が一部戻る。人間に了承を取ってから作る。
- U2 が戻らない・副作用がある → (b) を自動で満たす方法は今のところ無い。人間と相談する (候補: 手で変えたら人が知らせる、など)。
- U1 が効かない → この方式ではループが回らない。方式から人間と相談し直す。

## 5. 残りの作業 (U1 が効く前提)

1. **AI への問い合わせを .hlx 向けにする** (`ai.rs`): スクショと座標をやめ、`preset::render` の文を渡し、`preset::Change { block, parameter, value }` の一覧で受け取る。スキーマ・プロンプト・`validate` を変え、`ai.rs` のテストと `ai_live_test.rs` (実 API) を直す。プロンプトを変えたら実 API のテストを走らせる (OpenRouter の `.env` を使い、`OPENAI_MODEL=google/gemini-3.8-flash` で上書きするのが前任のやり方)。エフェクト一覧の検索ツール (`search_effects`) はそのまま使える。
2. **サイクル・Undo・ループに組み込む** (`workflow.rs`): ループの開始 (と手動の送信の開始) で `hx_io::export` → 中身をアプリが覚える → AI → `preset::apply` → 書き出し先のフォルダに書く → `hx_io::import`。Undo は前のファイルを `hx_io::import`。書き出し先は、アプリのデータのフォルダ (フォルダ名は書き出しの画面の「場所」で照合するので、他と重ならない名前にする)。`workflow_test.rs` の `FakeEngine` を新しい流れに合わせる。
3. **古い画面方式を外す**: 実機で新しい方式が動いたら、`automation.rs` (enigo の操作とガードレール)・`value_fields.rs`・`screen_check.rs`・スクショ (`capture.rs`) の使い道・模擬画面 (`scripts/make_mock_screenshot.swift`・`tests/fixtures/mock_hx_edit.png`)・`.cargo/mutants.toml` の除外を見直す。消すときは、何のためにあったかを確かめ、履歴に残っていることを人間に伝える。
4. README.md と AGENTS.md (テストの置き場所の表) を新しい作りに合わせる。
5. 必要なら `ai.rs` の「Undo は打ち直し」などの古い前提をメモリと README から更新する。

## 6. 実機と HX Edit について分かっていること

- 機器: HX Stomp XL。HX Edit 3.82 (`/Applications/Line6/HX Edit.app`、バンドル ID `com.line6.hxedit`)。.hlx の `device` は 2162699。
- 実験に使うスロットは **28B** (人間が空きとして使ってよいとしたもの)。今の中身は Tone Sovereign → Brit J45 Brt。往復テストが途中で止まったので、U1 の確認をしていなければ Gain 1 が 3.2 のまま。
- HX Edit は今のプリセットをディスクに置いていない (`~/Library/Application Support/Line 6` は設定とライセンスだけ)。中身を知る方法は書き出しだけ。
- アクセシビリティで見えるもの: メニューバー (全項目と有効・無効)、標準の保存・開くの画面、確認ダイアログ。本体のウィンドウの中 (プリセットの一覧・パラメータ) は見えない。
- メニューのショートカット: Save Preset ⌘S、Save Preset As ⇧⌘S、Import Preset ⌘I、Export Preset ⌘E、Import Setlist ⌥⌘I、Export Setlist ⌥⌘E。Edit メニューには Copy・Paste・Select All・Rename Preset などがある。
- 書き出しの画面 (シート) の部品の ID: `save-panel` (desc "export preset file")、`where popup`、`saveAsNameTextField`、`OKButton` (Save)、`CancelButton`。Cmd+Shift+G で `GoToWindow` が重なって開き、中に `PathTextField` と `CloseButton` がある。
- メニューの Import Preset... の画面は `open-panel` (desc "import preset file(s)")、`OKButton` は Open。今の作りでは使っていない (`open` で読み込むので)。
- `open` で .hlx を開いたときの確認: "Are you sure you want to overwrite the current preset with: <ファイルのパス>?" と No / Yes。パスは折り返して表示される。
- 画面の作りを写し取る台本: `scripts/probe_hx_edit_dialogs.swift` (書き出しと読み込みの画面を開いて木を出力し、Escape で閉じる。何も保存しない)。前回の出力は人間の `~/Downloads/hx_dialogs.txt`。

### 6.1 実機でつまずいたこと (同じ失敗をしないために)

- 書き出しの画面は、HX Edit が最前面でないと開かない → 操作の前に最前面にする。
- 「最前面か」を NSWorkspace の `frontmostApplication` で見ると、cargo test のような画面を持たない処理では更新されず古いまま ("Code") だった → HX Edit の `AXFrontmost` を見る。アプリ本体 (`macos::activate`) は画面を持つので NSWorkspace のままでよい。
- Cmd+Shift+G を `CGEventPostToPid` で HX Edit に送ると保存の画面に届かない → `kCGHIDEventTap` (キーボードと同じ経路) で、送る直前に最前面かを確かめて送る。
- `PathTextField` に AXValue を入れて `AXConfirm` しても「フォルダへ移動」は確定しない → Return キーを送る。
- メニューの有効・無効 (`AXEnabled`) は正しかった。前任は一度「表示が古いだけ」と見立てて外した。メニューが灰色なら本当に押せない (F1)。

## 7. 検証のしかた

```sh
npm run check                                  # すべての検査 (型・フロントのテスト・fmt・clippy・Rust のテスト)
cd src-tauri && cargo test --lib hx_io         # 書き出し・読み込みの流れ (偽の HX Edit)
cd src-tauri && cargo test --lib preset        # .hlx の読み書き
cd src-tauri && cargo test --lib ax_tree       # 画面の作りから部品を探す
```

実機の往復テスト (人間に走らせてもらう。選んでいるスロットを上書きし、最後に戻す。HX Edit で 28B を選び、終わるまでマウスとキーボードに触らないよう伝える):

```sh
cd ~/ghq/github.com/k-wa-wa/hx-ai/src-tauri && HX_EDIT_LIVE=1 cargo test hx_edit_round_trip -- --ignored --nocapture
```

このテストは保存をしない (F1 を切り分けるため外した)。今は「読み込みの後の書き出し」で F2 により止まる。U1 の結果によっては、読み込みの後に書き出して確かめる作り自体を見直す (ループでは書き出さない前提にするなら、テストも「書き出し → 書き換え → 読み込み → 人が画面で確かめる」に変える)。

## 8. 前任の進め方の反省

- 人間に英語で応答し続けた。必ず日本語で書く。
- 「筋が悪い」と 2 度言われた。1 回目は画面を読む方式、2 回目は自動の書き出しを 1 段ずつ直しては実機で走らせてもらう進め方。大きな方式を作る前に、HX Edit の振る舞い (保存や読み込みの後に何が押せるか、など) を人間に手で確かめてもらうのが早い。
- 状況を聞かれたら、「目的 → できたこと → 詰まっていること → 次の手」の順に表で答えると伝わった。
