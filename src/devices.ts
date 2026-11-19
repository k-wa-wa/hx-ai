import { InputDevice } from "./api";

/** 前回選んだデバイス → HX Stomp → OS の既定 → 先頭 の順に選ぶ。 */
export function pickDevice(devices: InputDevice[], saved: string | null): string | null {
  if (saved && devices.some((d) => d.id === saved)) return saved;
  const stomp = devices.find((d) => /hx\s*stomp/i.test(d.name));
  if (stomp) return stomp.id;
  return devices.find((d) => d.is_default)?.id ?? devices[0]?.id ?? null;
}
