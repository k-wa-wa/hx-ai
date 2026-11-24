import { describe, expect, it } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import SettingsWindow from "./SettingsWindow";
import { GRANTED, SETTINGS, setupBackend } from "./test/backend";

async function renderSettings() {
  const user = userEvent.setup();
  render(<SettingsWindow />);
  await screen.findByText("macOS の権限");
  await screen.findByRole("button", { name: "保存" });
  return user;
}

describe("macOS の権限", () => {
  it("足りない権限だけに「許可する」を出す", async () => {
    setupBackend({ permission_status: () => ({ ...GRANTED, accessibility: "denied" }) });
    await renderSettings();
    expect(await screen.findAllByRole("button", { name: "許可する" })).toHaveLength(1);
    expect(screen.getAllByText("許可済み")).toHaveLength(2);
  });

  it("「許可する」で許可を求め、少し後に状態を確かめ直す", async () => {
    let status = { ...GRANTED, screen_recording: "denied" };
    const b = setupBackend({
      permission_status: () => status,
      grant_permission: () => {
        status = GRANTED;
        return null;
      },
    });
    const user = await renderSettings();
    await user.click(await screen.findByRole("button", { name: "許可する" }));
    expect(b.callsTo("grant_permission")).toEqual([{ kind: "screen_recording" }]);
    await waitFor(() => expect(screen.queryByRole("button", { name: "許可する" })).not.toBeInTheDocument());
    expect(screen.getAllByText("許可済み")).toHaveLength(3);
  });

  it("↻ で状態を確かめ直す", async () => {
    const b = setupBackend({ permission_status: () => ({ ...GRANTED, microphone: "not_determined" }) });
    const user = await renderSettings();
    const before = b.callsTo("permission_status").length;
    await user.click(screen.getByRole("button", { name: "再確認" }));
    await waitFor(() => expect(b.callsTo("permission_status").length).toBe(before + 1));
  });

  it("許可を求められないときはエラーを出す", async () => {
    setupBackend({
      permission_status: () => ({ ...GRANTED, microphone: "denied" }),
      grant_permission: () => {
        throw "システム設定を開けません";
      },
    });
    const user = await renderSettings();
    await user.click(await screen.findByRole("button", { name: "許可する" }));
    expect(await screen.findByText("システム設定を開けません")).toBeInTheDocument();
  });
});

describe("AI の設定", () => {
  it("API キーが無ければそう表示する", async () => {
    setupBackend({ get_settings: () => ({ ...SETTINGS, api_key_source: "missing" }) });
    await renderSettings();
    expect(await screen.findByText("API キーがありません")).toBeInTheDocument();
  });

  it("キー欄が空なら保存済みのキーを変えない (null を送る)", async () => {
    const b = setupBackend({ save_settings: () => SETTINGS });
    const user = await renderSettings();
    await user.type(screen.getByLabelText("モデル"), "other-model");
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(b.callsTo("save_settings")).toHaveLength(1));
    expect(b.callsTo("save_settings")[0]).toEqual({
      input: { apiBaseUrl: "", apiKey: null, model: "other-model", targetApp: "" },
    });
  });

  it("入力したキーを送り、送った後は欄を空にして保存したと表示する", async () => {
    const b = setupBackend({ save_settings: () => SETTINGS });
    const user = await renderSettings();
    await user.type(screen.getByLabelText("API キー"), "sk-new");
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(b.callsTo("save_settings")).toHaveLength(1));
    expect(b.callsTo("save_settings")[0]).toMatchObject({ input: { apiKey: "sk-new" } });
    expect(screen.getByLabelText("API キー")).toHaveValue("");
    expect(await screen.findByText("保存した")).toBeInTheDocument();
  });

  it("「保存したキーを消す」なら空文字を送る", async () => {
    const withKey = { ...SETTINGS, stored: { ...SETTINGS.stored, has_api_key: true } };
    const b = setupBackend({ get_settings: () => withKey, save_settings: () => SETTINGS });
    const user = await renderSettings();
    await user.click(screen.getByLabelText("保存したキーを消す"));
    await user.click(screen.getByRole("button", { name: "保存" }));
    await waitFor(() => expect(b.callsTo("save_settings")).toHaveLength(1));
    expect(b.callsTo("save_settings")[0]).toMatchObject({ input: { apiKey: "" } });
  });

  it("保存できなければエラーを出す", async () => {
    setupBackend({
      save_settings: () => {
        throw "設定ファイルに書けません";
      },
    });
    const user = await renderSettings();
    await user.click(screen.getByRole("button", { name: "保存" }));
    expect(await screen.findByText("設定ファイルに書けません")).toBeInTheDocument();
  });
});
