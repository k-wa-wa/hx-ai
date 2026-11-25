import { InputDevice } from "./api";

/** HX Stomp / Helix など Line 6 のオーディオ入力デバイスか。 */
export function isHxAudioDevice(name: string): boolean {
  return /hx\s*stomp|helix|hx\s*effects|line\s*6/i.test(name);
}

/** `preferHx` が真なら接続された HX デバイスを優先し、そうでなければ前回 → HX Stomp → 既定 → 先頭。 */
export function pickDevice(
  devices: InputDevice[],
  saved: string | null,
  preferHx = false,
): string | null {
  const stomp = devices.find((d) => isHxAudioDevice(d.name));
  if (preferHx && stomp) return stomp.id;
  if (saved && devices.some((d) => d.id === saved)) return saved;
  if (stomp) return stomp.id;
  return devices.find((d) => d.is_default)?.id ?? devices[0]?.id ?? null;
}
