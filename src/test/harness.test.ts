import { invoke } from "@tauri-apps/api/core";
import { mockIPC } from "@tauri-apps/api/mocks";
import { describe, expect, it } from "vitest";

// Guards the test harness itself: if jsdom, the Tauri IPC mock, or the
// setup file's baseline wiring regresses, every other suite starts failing
// for reasons that have nothing to do with the code under test. This fails
// first and points straight at the harness.
describe("test harness", () => {
  it("runs in a DOM environment", () => {
    expect(typeof window).toBe("object");
    expect(document.createElement("div")).toBeInstanceOf(HTMLElement);
  });

  it("routes invoke() through a per-test IPC mock", async () => {
    mockIPC((cmd, args) => {
      if (cmd === "echo") return args;
      throw new Error(`unexpected command: ${cmd}`);
    });

    await expect(invoke("echo", { value: 42 })).resolves.toEqual({ value: 42 });
  });
});
