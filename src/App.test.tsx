import { describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { emit } from "@tauri-apps/api/event";
import App, { MAX_RECORD_SECONDS } from "./App";
import { GRANTED, HX_CONNECTED, report, SETTINGS, setupBackend } from "./test/backend";


async function renderApp() {
  const user = userEvent.setup();
  render(<App />);
  // 起動時の読み込み (デバイス一覧) を待つ
  await screen.findByRole("option", { name: /HX Stomp/ });
  return user;
}

describe("起動時", () => {
  it("HX Stomp を入力デバイスとして選ぶ", async () => {
    setupBackend();
    await renderApp();
    expect(screen.getByTitle("オーディオ入力")).toHaveValue("coreaudio:stomp");
  });

  it("前回選んだデバイスがあればそれを選ぶ", async () => {
    localStorage.setItem("deviceId", "coreaudio:builtin");
    setupBackend();
    await renderApp();
    expect(screen.getByTitle("オーディオ入力")).toHaveValue("coreaudio:builtin");
  });

  it("権限や API キーが無くても設定を勝手に開かない", async () => {
    const b = setupBackend({
      get_settings: () => ({ ...SETTINGS, api_key_source: "missing" }),
      permission_status: () => ({ ...GRANTED, microphone: "denied" }),
    });
    await renderApp();
    expect(b.callsTo("open_settings_window")).toEqual([]);
  });

  it("常に手前に表示は使わない", async () => {
    const b = setupBackend();
    await renderApp();
    expect(b.callsTo("plugin:window|set_always_on_top")).toEqual([]);
  });
});

describe("設定・USB 状態", () => {
  it("⚙︎ で設定ウィンドウを開く", async () => {
    const b = setupBackend({ open_settings_window: () => null });
    const user = await renderApp();
    await user.click(screen.getByTitle("設定"));
    await waitFor(() => expect(b.callsTo("open_settings_window")).toHaveLength(1));
    expect(screen.queryByLabelText("モデル")).not.toBeInTheDocument();
  });

  it("USB 未接続と接続中・解放を表示する", async () => {
    let status = { connected: false, device_name: null as string | null, session_open: false };
    const b = setupBackend({
      hx_status: () => status,
      disconnect_hx: () => ({ ...HX_CONNECTED, session_open: false }),
    });
    const user = await renderApp();
    expect(await screen.findByText("HX Stomp 未接続")).toBeInTheDocument();

    status = { ...HX_CONNECTED, session_open: true };
    await user.click(screen.getByRole("button", { name: "↻" }));
    expect(await screen.findByText("HX Stomp XL 接続中")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "USB 解放" }));
    await waitFor(() => expect(b.callsTo("disconnect_hx")).toHaveLength(1));
    expect(await screen.findByText("HX Stomp XL 待機中")).toBeInTheDocument();
  });
});

const SEND = "送信";
const RECORDED = { seconds: 4.2, peak_dbfs: -12.3 };

async function record(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "● 録音" }));
  await user.click(await screen.findByRole("button", { name: /■ 停止/ }));
  await screen.findByText(/🎙 録音 4.2 秒/);
}

describe("手動モード", () => {
  it("録音して止めると添付として表示し、指示と一緒に送る", async () => {
    const b = setupBackend({
      start_recording: () => null,
      stop_recording: () => RECORDED,
      send_message: () =>
        report({
          changes: [
            {
              block: "Amp",
              parameter: "Treble",
              previous_value: "7.2",
              value: "6.0",
            },
          ],
          undo_depth: 1,
        }),
    });
    const user = await renderApp();
    await record(user);
    expect(b.callsTo("start_recording")).toEqual([{ deviceId: "coreaudio:stomp" }]);
    expect(screen.getByText("🎙 録音 4.2 秒 · ピーク -12.3 dBFS")).toBeInTheDocument();

    await user.type(screen.getByRole("textbox"), "ハイを削って");
    await user.click(screen.getByRole("button", { name: SEND }));

    expect(await screen.findByText("高域を下げた")).toBeInTheDocument();
    expect(b.callsTo("send_message")).toEqual([{ instruction: "ハイを削って" }]);
    expect(screen.getByText("Amp / Treble")).toBeInTheDocument();
    expect(screen.getByText(/録音ピーク -12.3 dBFS/)).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("");
    expect(screen.queryByText(/🎙 録音 4.2 秒 · ピーク/)).not.toBeInTheDocument();
  });

  it("録音中は送れず、録音だけでも送れる", async () => {
    const b = setupBackend({
      start_recording: () => null,
      stop_recording: () => RECORDED,
      send_message: () => report(),
    });
    const user = await renderApp();
    expect(screen.getByRole("button", { name: SEND })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: "● 録音" }));
    await user.type(screen.getByRole("textbox"), "太く");
    expect(screen.getByRole("button", { name: SEND })).toBeDisabled();
    await user.click(screen.getByRole("button", { name: /■ 停止/ }));
    await user.clear(screen.getByRole("textbox"));
    await user.click(await screen.findByRole("button", { name: SEND }));
    await waitFor(() => expect(b.callsTo("send_message")).toEqual([{ instruction: "" }]));
  });

  it("録音なしで指示だけを送れる", async () => {
    const b = setupBackend({ send_message: () => report() });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "ディレイを足して");
    await user.click(screen.getByRole("button", { name: SEND }));
    await waitFor(() => expect(b.callsTo("send_message")).toHaveLength(1));
    expect(b.callsTo("start_recording")).toEqual([]);
  });

  it("使われなかった録音は添付に残す", async () => {
    setupBackend({
      start_recording: () => null,
      stop_recording: () => RECORDED,
      send_message: () => report({ explanations: [], error: "HTTP 503", recording_kept: true }),
    });
    const user = await renderApp();
    await record(user);
    await user.click(screen.getByRole("button", { name: SEND }));
    expect(await screen.findByText("HTTP 503")).toBeInTheDocument();
    expect(screen.getByText(/🎙 録音 4.2 秒 · ピーク/)).toBeInTheDocument();
  });

  it("添付した録音を消せる", async () => {
    const b = setupBackend({
      start_recording: () => null,
      stop_recording: () => RECORDED,
      discard_recording: () => null,
    });
    const user = await renderApp();
    await record(user);
    await user.click(screen.getByRole("button", { name: "録音を消す" }));
    await waitFor(() => expect(screen.queryByText(/🎙 録音 4.2 秒/)).not.toBeInTheDocument());
    expect(b.callsTo("discard_recording")).toHaveLength(1);
  });

  it("録音を始められないときはエラーを出し、録音中にならない", async () => {
    setupBackend({
      start_recording: () => {
        throw "マイクの権限がありません";
      },
    });
    const user = await renderApp();
    await user.click(screen.getByRole("button", { name: "● 録音" }));
    expect(await screen.findByText("マイクの権限がありません")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "● 録音" })).toBeEnabled();
  });

  it(`録音は ${MAX_RECORD_SECONDS} 秒で自動的に止まる`, async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    try {
      const b = setupBackend({ start_recording: () => null, stop_recording: () => RECORDED });
      const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
      render(<App />);
      await screen.findByRole("option", { name: /HX Stomp/ });
      await user.click(screen.getByRole("button", { name: "● 録音" }));
      await act(async () => {
        vi.advanceTimersByTime((MAX_RECORD_SECONDS - 1) * 1000);
      });
      expect(b.callsTo("stop_recording")).toHaveLength(0);
      await act(async () => {
        vi.advanceTimersByTime(1500);
      });
      await waitFor(() => expect(b.callsTo("stop_recording")).toHaveLength(1));
      expect(await screen.findByText(/🎙 録音 4.2 秒/)).toBeInTheDocument();
    } finally {
      vi.useRealTimers();
    }
  });

  it("⌘↩ で送る", async () => {
    const b = setupBackend({ send_message: () => report() });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "太く{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(b.callsTo("send_message")).toEqual([{ instruction: "太く" }]));
  });

  it("失敗したらエラーだけを出し、指示を入力欄に戻して送り直せるようにする", async () => {
    setupBackend({
      send_message: () => {
        throw "画面収録 の権限がありません";
      },
    });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "太く");
    await user.click(screen.getByRole("button", { name: SEND }));
    expect(await screen.findByText("画面収録 の権限がありません")).toBeInTheDocument();
    expect(screen.queryByText("変更はなかった。")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("太く");
    expect(screen.getByRole("button", { name: SEND })).toBeEnabled();
  });

  it("途中で止まったら、ここまでの変更を戻せると伝える", async () => {
    setupBackend({
      send_message: () =>
        report({
          changes: [
            { block: "Amp", parameter: "Bass", previous_value: "5", value: "6" },
          ],
          error: "マウスが動かされた",
          undo_depth: 1,
        }),
    });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "太く");
    await user.click(screen.getByRole("button", { name: SEND }));
    expect(await screen.findByText(/マウスが動かされた/)).toHaveTextContent(
      "ここまでの 1 件の変更は「元に戻す」で戻せる",
    );
  });

  it("AI に届く前に止まったときはエラーだけを出し、指示を戻す", async () => {
    setupBackend({ send_message: () => report({ explanations: [], error: "HTTP 503" }) });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "太く");
    await user.click(screen.getByRole("button", { name: SEND }));
    expect(await screen.findByText("HTTP 503")).toBeInTheDocument();
    expect(screen.queryByText("変更はなかった。")).not.toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("太く");
  });

  it("値を変えた後に止まったときは、指示を戻さない (送り直すと二重に変わる)", async () => {
    setupBackend({
      send_message: () =>
        report({
          changes: [
            { block: "Amp", parameter: "Bass", previous_value: "5", value: "6" },
          ],
          error: "マウスが動かされた",
          undo_depth: 1,
        }),
    });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "太く");
    await user.click(screen.getByRole("button", { name: SEND }));
    await screen.findByText(/マウスが動かされた/);
    expect(screen.getByRole("textbox")).toHaveValue("");
  });

  it("処理中は進み具合を表示し、ボタンを押せない", async () => {
    let finish: (r: unknown) => void = () => {};
    setupBackend({ send_message: () => new Promise((r) => (finish = r)) });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "太く");
    await user.click(screen.getByRole("button", { name: SEND }));
    await emit("progress", { stage: "ai", message: "AI に問い合わせ中" });
    expect(await screen.findByText("AI に問い合わせ中")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "処理中…" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "● 録音" })).toBeDisabled();
    finish(report());
    expect(await screen.findByText("高域を下げた")).toBeInTheDocument();
    expect(screen.queryByText("AI に問い合わせ中")).not.toBeInTheDocument();
  });
});

describe("自動ループ", () => {
  async function renderAuto() {
    const user = await renderApp();
    await user.click(screen.getByRole("radio", { name: "自動ループ" }));
    return user;
  }

  it("目標・デバイス・録音秒数・上限を渡して始め、毎回の結果と終わった理由を表示する", async () => {
    let finish: (r: unknown) => void = () => {};
    let depth = 0;
    const b = setupBackend({
      start_loop: () => new Promise((r) => (finish = r)),
      undo_depth: () => depth,
    });
    const user = await renderAuto();
    await user.click(screen.getByRole("radio", { name: "3 秒" }));
    await user.click(screen.getByRole("radio", { name: "20 回" }));
    await user.type(screen.getByRole("textbox"), "リファレンスに近づけて");
    await user.click(screen.getByRole("button", { name: "▶ 自動ループを始める" }));

    await waitFor(() =>
      expect(b.callsTo("start_loop")).toEqual([
        { instruction: "リファレンスに近づけて", deviceId: "coreaudio:stomp", seconds: 3, maxIterations: 20 },
      ]),
    );
    await emit("loop_iteration", {
      iteration: 1,
      max: 20,
      report: report({ explanations: ["Bass を上げた"], undo_depth: 1 }),
    });
    expect(await screen.findByText("Bass を上げた")).toBeInTheDocument();
    expect(screen.getByText(/ループ 1\/20/)).toBeInTheDocument();
    await emit("loop_iteration", {
      iteration: 2,
      max: 20,
      report: report({ explanations: ["これで十分近い"], goal_reached: true, undo_depth: 1 }),
    });
    expect(await screen.findByText(/目標に届いた/)).toBeInTheDocument();

    depth = 1;
    finish({ iterations: 2, reason: "AI が目標に届いたと判断した", undo_depth: 1 });
    expect(
      await screen.findByText("自動ループを終えた (2 回): AI が目標に届いたと判断した"),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "▶ 自動ループを始める" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "↶ 戻す (1)" })).toBeEnabled();
  });

  it("ループ中は止められ、指示を足せる。戻すは押せない", async () => {
    let finish: (r: unknown) => void = () => {};
    const b = setupBackend({
      start_loop: () => new Promise((r) => (finish = r)),
      stop_loop: () => null,
      add_loop_instruction: () => null,
      undo_depth: () => 1,
    });
    const user = await renderAuto();
    await user.click(screen.getByRole("button", { name: "▶ 自動ループを始める" }));
    const stop = await screen.findByRole("button", { name: "■ ループを止める" });
    expect(screen.getByRole("button", { name: /戻す/ })).toBeDisabled();

    await user.type(screen.getByRole("textbox"), "もう少し歪ませて");
    await user.click(screen.getByRole("button", { name: "指示を追加" }));
    await waitFor(() => expect(b.callsTo("add_loop_instruction")).toEqual([{ text: "もう少し歪ませて" }]));
    expect(screen.getByText(/次の回から加える/)).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("");

    await user.click(stop);
    await waitFor(() => expect(b.callsTo("stop_loop")).toHaveLength(1));
    expect(screen.getByRole("button", { name: "止めています…" })).toBeDisabled();
    finish({ iterations: 1, reason: "停止ボタンで止めた", undo_depth: 1 });
    expect(await screen.findByText(/停止ボタンで止めた/)).toBeInTheDocument();
  });

  it("⌘↩ でループ中は指示を足す", async () => {
    const b = setupBackend({
      start_loop: () => new Promise(() => {}),
      add_loop_instruction: () => null,
    });
    const user = await renderAuto();
    await user.type(screen.getByRole("textbox"), "目標{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(b.callsTo("start_loop")).toHaveLength(1));
    await user.type(screen.getByRole("textbox"), "追加{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(b.callsTo("add_loop_instruction")).toEqual([{ text: "追加" }]));
  });

  it("始められないときはエラーを出し、元の状態に戻る", async () => {
    setupBackend({
      start_loop: () => {
        throw "HX Stomp / HX Stomp XL が USB で見つかりません";
      },
    });
    const user = await renderAuto();
    await user.click(screen.getByRole("button", { name: "▶ 自動ループを始める" }));
    expect(await screen.findByText(/USB で見つかりません/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "▶ 自動ループを始める" })).toBeEnabled();
  });

  it("モードを覚えておく", async () => {
    setupBackend();
    await renderAuto();
    expect(localStorage.getItem("mode")).toBe("auto");
  });
});

describe("元に戻す", () => {
  it("戻せる変更が無いうちは押せず、あれば undo_last を呼ぶ", async () => {
    let depth = 0;
    const b = setupBackend({
      undo_depth: () => depth,
      send_message: () => report({ undo_depth: (depth = 1) }),
      undo_last: () => report({ explanations: [], undo_depth: (depth = 0) }),
    });
    const user = await renderApp();
    const undo = screen.getByRole("button", { name: /戻す/ });
    expect(undo).toBeDisabled();

    await user.type(screen.getByRole("textbox"), "太く");
    await user.click(screen.getByRole("button", { name: "送信" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "↶ 戻す (1)" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "↶ 戻す (1)" }));
    expect(await screen.findByText("直前の変更を元に戻した。")).toBeInTheDocument();
    expect(b.callsTo("undo_last")).toHaveLength(1);
  });

  it("起動時に Rust 側に残っている Undo 履歴の数を取りに行く", async () => {
    setupBackend({ undo_depth: () => 2 });
    await renderApp();
    expect(await screen.findByRole("button", { name: "↶ 戻す (2)" })).toBeEnabled();
  });

  it("戻せなかった値を表示する", async () => {
    setupBackend({
      undo_depth: () => 1,
      undo_last: () =>
        report({
          explanations: [],
          error: "1 個の値を戻せませんでした",
          not_restored: [{ block: "Amp", parameter: "Bass", value: "5.0" }],
          undo_depth: 1,
        }),
    });
    const user = await renderApp();
    await waitFor(() => expect(screen.getByRole("button", { name: /戻す/ })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: /戻す/ }));
    expect(await screen.findByText("5.0 に戻せていない")).toBeInTheDocument();
  });
});

describe("リファレンス", () => {
  const REF = {
    name: "song.mp3",
    mime: "audio/mpeg",
    size: 1000,
    duration: 192,
    clipStart: 0,
    clipSeconds: 20,
  };

  async function drop() {
    await emit("tauri://drag-drop", { paths: ["/music/song.mp3"], position: { x: 0, y: 0 } });
  }

  it("落とした音源を読み込み、AI に送る区間を表示する", async () => {
    const b = setupBackend({
      set_reference: () => REF,
      get_reference_audio: () => new ArrayBuffer(4),
    });
    await renderApp();
    await drop();
    expect(await screen.findByText("AI に送る区間 0:00〜0:20")).toBeInTheDocument();
    expect(b.callsTo("set_reference")).toEqual([{ path: "/music/song.mp3" }]);
  });

  it("再生位置から区間を選び直す", async () => {
    const b = setupBackend({
      set_reference: () => REF,
      get_reference_audio: () => new ArrayBuffer(4),
      set_reference_start: (args) => ({ ...REF, clipStart: (args as { seconds: number }).seconds }),
    });
    const user = await renderApp();
    await drop();
    await screen.findByText("AI に送る区間 0:00〜0:20");
    const audio = document.querySelector("audio")!;
    Object.defineProperty(audio, "currentTime", { value: 75.4, configurable: true });
    await user.click(screen.getByRole("button", { name: "再生位置から使う" }));
    expect(await screen.findByText("AI に送る区間 1:15〜1:35")).toBeInTheDocument();
    expect(b.callsTo("set_reference_start")).toEqual([{ seconds: 75.4 }]);
  });

  it("区間を選べないときはエラーを出し、前の区間の表示を残す", async () => {
    setupBackend({
      set_reference: () => REF,
      get_reference_audio: () => new ArrayBuffer(4),
      set_reference_start: () => {
        throw "リファレンスの 1:15 からの区間がほぼ無音です";
      },
    });
    const user = await renderApp();
    await drop();
    await user.click(await screen.findByRole("button", { name: "再生位置から使う" }));
    expect(await screen.findByText(/ほぼ無音です/)).toBeInTheDocument();
    expect(screen.getByText("AI に送る区間 0:00〜0:20")).toBeInTheDocument();
  });

  it("20 秒より短い音源は全体を送るので、選び直すボタンを出さない", async () => {
    setupBackend({
      set_reference: () => ({ ...REF, duration: 8.5, clipSeconds: 8.5 }),
      get_reference_audio: () => new ArrayBuffer(4),
    });
    await renderApp();
    await drop();
    expect(await screen.findByText("AI に送る区間 0:00〜0:08")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "再生位置から使う" })).not.toBeInTheDocument();
  });

  it("読めない音源はエラーを出す", async () => {
    setupBackend({
      set_reference: () => {
        throw "音源ファイルを読めません";
      },
    });
    await renderApp();
    await drop();
    expect(await screen.findByText("音源ファイルを読めません")).toBeInTheDocument();
  });
});
