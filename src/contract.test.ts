// フロントエンドと Rust の取り決めがずれていないかを、ソースを読んで確かめる。
// 片方だけ名前を変えると、ビルドは通っても実行時に失敗するため。

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const root = resolve(__dirname, "..");
const read = (p: string) => readFileSync(resolve(root, p), "utf8");

/** api.ts で invoke しているコマンド名。 */
function frontendCommands(): string[] {
  return [...read("src/api.ts").matchAll(/invoke<[^>]*>\("([a-z_]+)"/g)].map((m) => m[1]);
}

/** lib.rs の generate_handler! に並んだコマンド名。 */
function registeredCommands(): string[] {
  const m = read("src-tauri/src/lib.rs").match(/generate_handler!\[([\s\S]*?)\]/);
  if (!m) throw new Error("generate_handler! が見つからない");
  return m[1]
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/** Rust の関数 `fn name(...)` の引数名 (State と AppHandle を除く)。 */
function rustParams(name: string): string[] {
  const src = read("src-tauri/src/lib.rs");
  const m = src.match(new RegExp(`fn ${name}\\(([\\s\\S]*?)\\)\\s*(->|\\{)`));
  if (!m) throw new Error(`fn ${name} が見つからない`);
  return m[1]
    .replace(/<[^<>]*>/g, "")
    .split(",")
    .map((p) => p.trim())
    .filter((p) => p && !/:\s*(State|AppHandle)\b/.test(p))
    .map((p) => p.split(":")[0].trim());
}

/** api.ts で invoke に渡している引数名。 */
function frontendArgs(cmd: string): string[] {
  const m = read("src/api.ts").match(new RegExp(`invoke<[^>]*>\\("${cmd}"(?:, \\{ ([^}]*) \\})?\\)`));
  if (!m) throw new Error(`${cmd} の invoke が見つからない`);
  return m[1] ? m[1].split(",").map((s) => s.split(":")[0].trim()) : [];
}

const camel = (snake: string) => snake.replace(/_([a-z])/g, (_, c: string) => c.toUpperCase());

describe("フロントエンドと Rust のコマンド", () => {
  it("呼んでいるコマンドはすべて Rust に登録されている", () => {
    const registered = new Set(registeredCommands());
    const missing = frontendCommands().filter((c) => !registered.has(c));
    expect(missing).toEqual([]);
  });

  it("登録したコマンドはすべて画面から使われている", () => {
    const used = new Set(frontendCommands());
    const unused = registeredCommands().filter((c) => !used.has(c));
    expect(unused).toEqual([]);
  });

  it("引数名が一致する (Rust の snake_case は JS では camelCase)", () => {
    for (const cmd of registeredCommands()) {
      const expected = rustParams(cmd).map(camel).sort();
      expect({ cmd, args: frontendArgs(cmd).sort() }).toEqual({ cmd, args: expected });
    }
  });

  it("グローバルショートカットの登録に必要な権限がある", () => {
    const perms: string[] = JSON.parse(read("src-tauri/capabilities/default.json")).permissions;
    for (const p of [
      "global-shortcut:allow-register",
      "global-shortcut:allow-unregister",
      "core:window:allow-set-always-on-top",
    ]) {
      expect(perms).toContain(p);
    }
  });
});
