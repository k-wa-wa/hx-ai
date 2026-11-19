import { describe, expect, it } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { emit } from "@tauri-apps/api/event";
import App from "./App";
import { GRANTED, report, SETTINGS, setupBackend } from "./test/backend";

const RUN = /聴いて調整する/;

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

  it("API キーが無ければ設定画面を開く", async () => {
    setupBackend({ get_settings: () => ({ ...SETTINGS, api_key_source: "missing" }) });
    await renderApp();
    expect(await screen.findByText("API キーがありません")).toBeInTheDocument();
  });

  it("権限が揃っていなければ権限の案内を開く", async () => {
    setupBackend({ permission_status: () => ({ ...GRANTED, accessibility: "denied" }) });
    await renderApp();
    expect(await screen.findByText("macOS の権限")).toBeInTheDocument();
    expect(screen.getAllByRole("button", { name: "設定を開く" })).toHaveLength(1);
  });

  it("グローバルショートカットを登録する", async () => {
    const b = setupBackend();
    await renderApp();
    await waitFor(() => expect(b.callsTo("plugin:global-shortcut|register")).toHaveLength(1));
    expect(b.callsTo("plugin:global-shortcut|register")[0]).toMatchObject({
      shortcuts: ["CommandOrControl+Alt+H"],
    });
  });

  it("常に手前に表示を既定でオンにし、切り替えられる", async () => {
    const b = setupBackend();
    const user = await renderApp();
    await waitFor(() =>
      expect(b.callsTo("plugin:window|set_always_on_top").slice(-1)[0]).toMatchObject({ value: true }),
    );
    await user.click(screen.getByTitle("常に手前に表示: オン"));
    await waitFor(() =>
      expect(b.callsTo("plugin:window|set_always_on_top").slice(-1)[0]).toMatchObject({ value: false }),
    );
    expect(localStorage.getItem("onTop")).toBe("false");
  });
});

describe("聴いて調整する", () => {
  it("指示・デバイス・録音秒数を渡し、結果の変更を表示する", async () => {
    const b = setupBackend({
      run_cycle: () =>
        report({
          changes: [
            {
              kind: "set_value",
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
    await user.click(screen.getByRole("radio", { name: "3 秒" }));
    await user.type(screen.getByRole("textbox"), "ハイを削って");
    await user.click(screen.getByRole("button", { name: RUN }));

    expect(await screen.findByText("高域を下げた")).toBeInTheDocument();
    expect(b.callsTo("run_cycle")).toEqual([
      { instruction: "ハイを削って", deviceId: "coreaudio:stomp", seconds: 3 },
    ]);
    expect(screen.getByText("Amp / Treble")).toBeInTheDocument();
    expect(screen.getByText("6.0")).toBeInTheDocument();
    expect(screen.getByText(/録音ピーク -12.3 dBFS/)).toBeInTheDocument();
    expect(screen.getByRole("textbox")).toHaveValue("");
  });

  it("⌘↩ でも実行する", async () => {
    const b = setupBackend({ run_cycle: () => report() });
    const user = await renderApp();
    await user.type(screen.getByRole("textbox"), "太く{Meta>}{Enter}{/Meta}");
    await waitFor(() => expect(b.callsTo("run_cycle")).toHaveLength(1));
    expect(b.callsTo("run_cycle")[0]).toMatchObject({ instruction: "太く" });
  });

  it("失敗したらエラーだけを出し、「変更はなかった」とは言わない", async () => {
    setupBackend({
      run_cycle: () => {
        throw "画面収録 の権限がありません";
      },
    });
    const user = await renderApp();
    await user.click(screen.getByRole("button", { name: RUN }));
    expect(await screen.findByText("画面収録 の権限がありません")).toBeInTheDocument();
    expect(screen.queryByText("変更はなかった。")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: RUN })).toBeEnabled();
  });

  it("途中で止まったら、ここまでの変更を戻せると伝える", async () => {
    setupBackend({
      run_cycle: () =>
        report({
          changes: [
            { kind: "set_value", block: "Amp", parameter: "Bass", previous_value: "5", value: "6" },
          ],
          error: "マウスが動かされた",
          undo_depth: 1,
        }),
    });
    const user = await renderApp();
    await user.click(screen.getByRole("button", { name: RUN }));
    expect(await screen.findByText(/マウスが動かされた/)).toHaveTextContent(
      "ここまでの 1 件の変更は「元に戻す」で戻せる",
    );
  });

  it("AI に届く前に止まったときはエラーだけを出す", async () => {
    setupBackend({ run_cycle: () => report({ explanations: [], error: "HTTP 503" }) });
    const user = await renderApp();
    await user.click(screen.getByRole("button", { name: RUN }));
    expect(await screen.findByText("HTTP 503")).toBeInTheDocument();
    expect(screen.queryByText("変更はなかった。")).not.toBeInTheDocument();
  });

  it("処理中は進み具合を表示し、ボタンを押せない", async () => {
    let finish: (r: unknown) => void = () => {};
    setupBackend({ run_cycle: () => new Promise((r) => (finish = r)) });
    const user = await renderApp();
    await user.click(screen.getByRole("button", { name: RUN }));
    await emit("progress", { stage: "record", message: "5 秒録音中" });
    expect(await screen.findByText("5 秒録音中")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "処理中…" })).toBeDisabled();
    finish(report());
    expect(await screen.findByRole("button", { name: RUN })).toBeEnabled();
    expect(screen.queryByText("5 秒録音中")).not.toBeInTheDocument();
  });
});

describe("元に戻す", () => {
  it("戻せる変更が無いうちは押せず、あれば undo_last を呼ぶ", async () => {
    let depth = 0;
    const b = setupBackend({
      undo_depth: () => depth,
      run_cycle: () => report({ undo_depth: (depth = 1) }),
      undo_last: () => report({ explanations: [], undo_depth: (depth = 0) }),
    });
    const user = await renderApp();
    const undo = screen.getByRole("button", { name: /戻す/ });
    expect(undo).toBeDisabled();

    await user.click(screen.getByRole("button", { name: RUN }));
    await waitFor(() => expect(screen.getByRole("button", { name: "↶ 戻す (1)" })).toBeEnabled());
    await user.click(screen.getByRole("button", { name: "↶ 戻す (1)" }));
    expect(await screen.findByText("直前の変更を元に戻した。")).toBeInTheDocument();
    expect(b.callsTo("undo_last")).toHaveLength(1);
  });

});

describe("設定", () => {
  async function openSettings() {
    const user = await renderApp();
    await user.click(screen.getByTitle("設定"));
    return user;
  }

  it("キー欄が空なら保存済みのキーを変えない (null を送る)", async () => {
    const b = setupBackend({ save_settings: () => SETTINGS });
    const user = await openSettings();
    await user.type(screen.getByLabelText("モデル"), "other-model");
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(b.callsTo("save_settings")).toHaveLength(1));
    expect(b.callsTo("save_settings")[0]).toEqual({
      input: { apiBaseUrl: "", apiKey: null, model: "other-model", targetApp: "" },
    });
  });

  it("入力したキーを送り、送った後は欄を空にする", async () => {
    const b = setupBackend({ save_settings: () => SETTINGS });
    const user = await openSettings();
    await user.type(screen.getByLabelText("API キー"), "sk-new");
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(b.callsTo("save_settings")).toHaveLength(1));
    expect(b.callsTo("save_settings")[0]).toMatchObject({ input: { apiKey: "sk-new" } });
    expect(screen.getByLabelText("API キー")).toHaveValue("");
  });

  it("「保存したキーを消す」なら空文字を送る", async () => {
    const withKey = { ...SETTINGS, stored: { ...SETTINGS.stored, has_api_key: true } };
    const b = setupBackend({ get_settings: () => withKey, save_settings: () => SETTINGS });
    const user = await openSettings();
    await user.click(screen.getByLabelText("保存したキーを消す"));
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(b.callsTo("save_settings")).toHaveLength(1));
    expect(b.callsTo("save_settings")[0]).toMatchObject({ input: { apiKey: "" } });
  });
});
