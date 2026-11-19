import { describe, expect, it } from "vitest";
import { pickDevice } from "./devices";
import type { InputDevice } from "./api";

const mic: InputDevice = { id: "mic", name: "MacBook Pro のマイク", is_default: true };
const stomp: InputDevice = { id: "stomp", name: "Line 6 HX Stomp", is_default: false };
const other: InputDevice = { id: "other", name: "USB Audio", is_default: false };

describe("pickDevice", () => {
  it("前回選んだデバイスがあれば最優先", () => {
    expect(pickDevice([mic, stomp], "mic")).toBe("mic");
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
