import { mockIPC } from "@tauri-apps/api/mocks";
import { render, screen } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * `InstanceWorlds` only touches the instances API layer (plain `invoke`
 * calls, no module-scope listeners), but the IPC mock must still exist
 * first — hence the dynamic import like the other specs.
 */
type WorldsModule = typeof import("./InstanceWorlds");

let WorldsTab: WorldsModule["WorldsTab"];
let ServersTab: WorldsModule["ServersTab"];

beforeEach(async () => {
  vi.resetModules();
  mockIPC((cmd) => {
    if (cmd === "list_pending_missing_mods") return [];
    if (cmd === "list_instance_worlds")
      return [
        {
          folderName: "New World",
          name: "My Adventure",
          lastPlayedMs: 1_700_000_000_000,
          gameMode: "Survival",
          gameVersion: "1.21.1",
        },
        { folderName: "corrupt-world" },
      ];
    if (cmd === "list_instance_servers")
      return [{ name: "Home", address: "play.example.com:25565" }];
    return undefined;
  });
  ({ WorldsTab, ServersTab } = await import("./InstanceWorlds"));
});

describe("WorldsTab", () => {
  it("lists worlds with details and degrades corrupt ones to folder names", async () => {
    render(<WorldsTab instanceId="inst-1" />);

    expect(await screen.findByText("My Adventure")).toBeInTheDocument();
    expect(screen.getByText(/Survival/)).toBeInTheDocument();
    expect(screen.getByText(/1\.21\.1/)).toBeInTheDocument();
    // Corrupt level.dat: the folder name still shows the row (as both
    // the name and the meta fallback).
    expect(screen.getAllByText("corrupt-world").length).toBeGreaterThan(0);
  });

  it("shows the empty state with no worlds", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "list_instance_worlds") return [];
      return undefined;
    });
    vi.resetModules();
    ({ WorldsTab } = await import("./InstanceWorlds"));
    render(<WorldsTab instanceId="inst-1" />);

    expect(await screen.findByText("No worlds yet")).toBeInTheDocument();
  });
});

describe("ServersTab", () => {
  it("lists servers with addresses", async () => {
    render(<ServersTab instanceId="inst-1" />);

    expect(await screen.findByText("Home")).toBeInTheDocument();
    expect(screen.getByText("play.example.com:25565")).toBeInTheDocument();
  });

  it("shows the empty state with no servers", async () => {
    mockIPC((cmd) => {
      if (cmd === "list_pending_missing_mods") return [];
      if (cmd === "list_instance_servers") return [];
      return undefined;
    });
    vi.resetModules();
    ({ ServersTab } = await import("./InstanceWorlds"));
    render(<ServersTab instanceId="inst-1" />);

    expect(await screen.findByText("No servers yet")).toBeInTheDocument();
  });
});
