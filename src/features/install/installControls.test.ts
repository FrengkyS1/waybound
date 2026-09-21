import { mockIPC } from "@tauri-apps/api/mocks";
import { describe, expect, it, vi } from "vitest";

describe("download pause acknowledgement", () => {
  it("does not claim a pause before acknowledgement and preserves paused state on failed resume", async () => {
    let acknowledge!: () => void;
    mockIPC((command) => {
      if (command === "list_pending_missing_mods") return [];
      if (command === "pause_install") return new Promise<void>((resolve) => { acknowledge = resolve; });
      if (command === "resume_install") throw new Error("Install is no longer available");
      return 1;
    });
    const { useInstallStore: store } = await import("./installStore");
    // Store registers IPC listeners at import time; install the mock first.
    store.setState({ installs: [{ id: "isolated", name: "Example", status: "installing", etaSeconds: 100 }] });
    store.getState().setPaused("isolated", true);
    expect(store.getState().installs[0].paused).not.toBe(true);
    expect(store.getState().installs[0].controlPending).toBe(true);
    acknowledge();
    await vi.waitFor(() => expect(store.getState().installs[0].paused).toBe(true));
    expect(store.getState().installs[0].etaSeconds).toBeUndefined();
    store.getState().setPaused("isolated", false);
    await vi.waitFor(() => expect(store.getState().installs[0].controlError).toContain("no longer available"));
    expect(store.getState().installs[0].paused).toBe(true);
    expect(store.getState().installs[0].controlPending).toBe(false);
  });
});
