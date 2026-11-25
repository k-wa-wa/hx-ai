import { describe, expect, it } from "vitest";
import { isHxAudioDevice, pickDevice } from "./devices";
import type { InputDevice } from "./api";

const mic: InputDevice = { id: "mic", name: "MacBook Pro のマイク", is_default: true };
const stomp: InputDevice = { id: "stomp", name: "Line 6 HX Stomp", is_default: false };
const other: InputDevice = { id: "other", name: "USB Audio", is_default: false };

describe("isHxAudioDevice", () => {
  it("HX 系と通常のマイクを見分ける", () => {
    expect(isHxAudioDevice("HX Stomp XL")).toBe(true);
    expect(isHxAudioDevice("Line 6 HX Stomp")).toBe(true);
    expect(isHxAudioDevice("MacBook Pro のマイク")).toBe(false);
  });
});

describe("pickDevice", () => {
  it("前回選んだデバイスがあれば最優先 (preferHx 時は HX へ切り替える)", () => {
    expect(pickDevice([mic, stomp], "mic")).toBe("mic");
    expect(pickDevice([mic, stomp], "mic", true)).toBe("stomp");
  });
  it("前回のデバイスが外されていたら HX Stomp", () => {
    expect(pickDevice([mic, stomp], "gone")).toBe("stomp");
  });
  it("名前の大小文字や空白の違いを気にせず HX Stomp を見つける", () => {
    expect(pickDevice([mic, { ...stomp, name: "hxstomp" }], null)).toBe("stomp");
  });
  it("HX Stomp が無ければ OS の既定", () => {
    expect(pickDevice([other, mic], null)).toBe("mic");
  });
  it("既定も無ければ先頭、空なら null", () => {
    expect(pickDevice([other], null)).toBe("other");
    expect(pickDevice([], null)).toBeNull();
  });
});
