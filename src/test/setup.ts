import "@testing-library/jest-dom/vitest";
import { cleanup } from "@testing-library/react";
import { clearMocks } from "@tauri-apps/api/mocks";
import { afterEach } from "vitest";

// jsdom に無いもの
Element.prototype.scrollIntoView = function () {};

afterEach(async () => {
  cleanup();
  // アンマウント時の購読解除 (非同期) が終わってからモックを消す
  await new Promise((r) => setTimeout(r, 0));
  clearMocks();
  localStorage.clear();
});
