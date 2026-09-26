import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * `PlayButton` pulls in the play store (module-scope event wiring), so it
 * loads dynamically after the IPC mock exists.
 */
type ButtonModule = typeof import("./PlayButton");
type StoreModule = typeof import("./store");

let PlayButton: ButtonModule["PlayButton"];
let usePlayStore: StoreModule["usePlayStore"];

interface Call {
  cmd: string;
  args: Record<string, unknown>;
}

let calls: Call[];

function renderButton() {
  render(<PlayButton instanceId="inst-1" instanceName="Test Instance" />);
}

beforeEach(async () => {
  vi.resetModules();
  calls = [];
  mockIPC((cmd, args) => {
    calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
    if (cmd === "list_pending_missing_mods") return [];
    if (cmd === "check_launch_readiness")
      return {
        checkedFiles: 2,
        wrongLoader: [
          { fileName: "formations.jar", modName: "Formations", detectedLoader: "neoforge" },
        ],
        missingDeps: [{ fileName: "a.jar", modName: "Mod A", depModId: "somelib" }],
      };
    if (cmd === "launch_instance") return undefined;
    return undefined;
  });
  ({ PlayButton } = await import("./PlayButton"));
  ({ usePlayStore } = await import("./store"));
  usePlayStore.setState({
    account: { uuid: "u", username: "Player" },
    launches: {},
  });
});

describe("PlayButton readiness gate", () => {
  it("warns about loader mismatch and missing deps, then launches on confirm", async () => {
    renderButton();

    fireEvent.click(screen.getByRole("button", { name: /play/i }));

    await waitFor(() =>
      expect(screen.getByText("Possible mod problems")).toBeInTheDocument(),
    );
    expect(
      screen.getByText(/"Formations" is a Neoforge mod — it won't load here/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/"Mod A" needs "somelib", which isn't installed/),
    ).toBeInTheDocument();
    // Nothing launched yet — the dialog gates it.
    expect(calls.filter((c) => c.cmd === "launch_instance")).toHaveLength(0);

    fireEvent.click(screen.getByRole("button", { name: /launch anyway/i }));

    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "launch_instance")).toHaveLength(1),
    );
  });

  it("launches straight through when the check is clean", async () => {
    mockIPC((cmd, args) => {
      calls.push({ cmd, args: (args ?? {}) as Record<string, unknown> });
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "check_launch_readiness")
        return { checkedFiles: 1, wrongLoader: [], missingDeps: [] };
      if (cmd === "launch_instance") return undefined;
      return undefined;
    });
    vi.resetModules();
    ({ PlayButton } = await import("./PlayButton"));
    ({ usePlayStore } = await import("./store"));
    usePlayStore.setState({
      account: { uuid: "u", username: "Player" },
      launches: {},
    });
    renderButton();

    fireEvent.click(screen.getByRole("button", { name: /play/i }));

    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "launch_instance")).toHaveLength(1),
    );
    expect(screen.queryByText("Possible mod problems")).not.toBeInTheDocument();
  });

  it("offers Cancel while preparing, wired to cancel_launch", async () => {
    usePlayStore.setState({
      account: { uuid: "u", username: "Player" },
      launches: {
        "inst-1": {
          instanceId: "inst-1",
          instanceName: "Test Instance",
          phase: "preparing",
          stage: "Downloading",
          current: 1,
          total: 4,
          logs: [],
          exitCode: null,
          error: null,
          startedAtMs: Date.now(),
          crashed: false,
          crashReason: null,
        },
      },
    });
    renderButton();

    fireEvent.click(screen.getByRole("button", { name: /^cancel$/i }));

    await waitFor(() =>
      expect(
        calls.filter((c) => c.cmd === "cancel_launch"),
      ).toHaveLength(1),
    );
    expect(calls.filter((c) => c.cmd === "launch_instance")).toHaveLength(0);
  });

  it("morphs to Stop while running, wired to stop_game", async () => {
    usePlayStore.setState({
      account: { uuid: "u", username: "Player" },
      launches: {
        "inst-1": {
          instanceId: "inst-1",
          instanceName: "Test Instance",
          phase: "running",
          stage: "Minecraft is running",
          current: 0,
          total: 0,
          logs: [],
          exitCode: null,
          error: null,
          startedAtMs: Date.now(),
          crashed: false,
          crashReason: null,
        },
      },
    });
    renderButton();

    fireEvent.click(screen.getByRole("button", { name: /^stop$/i }));

    await waitFor(() =>
      expect(calls.filter((c) => c.cmd === "stop_game")).toHaveLength(1),
    );
  });
});
