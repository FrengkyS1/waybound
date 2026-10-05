import { mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { LoaderVersionInfo } from "../instances/api";
import type { InstanceSummary } from "../instances/types";
import { LaunchOverrides } from "./LaunchOverrides";

const instance: InstanceSummary = {
  id: "forge-instance", name: "Forge instance", minecraftVersion: "1.20.1",
  loader: "forge", loaderVersion: "47.2.99", modCount: 0, createdAt: 0,
  rootPath: "C:/throwaway/forge", totalPlaySeconds: 0,
};

let info: LoaderVersionInfo;
let commands: string[];

beforeEach(() => {
  info = {
    loader: "forge", minecraftVersion: "1.20.1", latest: "47.2.100",
    recommended: "47.2.0", fromCache: false, fetchedAtUnix: 1,
  };
  commands = [];
  mockIPC((command) => {
    commands.push(command);
    if (command === "get_loader_version_info") return info;
    if (command === "get_launch_settings") {
      return { detected: [], javaPath: null, maxMemoryMb: 4096, jvmArgs: null };
    }
    if (command === "get_instance_launch_config") {
      return { javaPath: null, maxMemoryMb: null, jvmArgs: null };
    }
    return null;
  });
});

describe("explicit Forge loader updates", () => {
  it("offers and applies latest rather than downgrading to recommended", async () => {
    const onLoaderVersionChange = vi.fn(async () => {});
    render(<LaunchOverrides instance={instance} onLoaderVersionChange={onLoaderVersionChange} />);
    fireEvent.click(screen.getByRole("button", { name: "Check for update" }));
    fireEvent.click(await screen.findByRole("button", { name: "Update to 47.2.100" }));
    expect(screen.queryByRole("button", { name: "Update to 47.2.0" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    await waitFor(() => expect(onLoaderVersionChange).toHaveBeenCalledWith("47.2.100"));
  });

  it.each([
    ["47.2.100", "47.2.99", true],
    ["47.2.99", "47.2.100", false],
    ["47.2.99", "47.2.99", false],
    ["47.2.99", "47.2.99.0", false],
    ["47.2.100", "1.20.1-47.2.99", true],
    ["47.2.99+release", "47.2.99", false],
    ["47.2.99", "47.2.99-beta.2", true],
    ["47.2.99-beta.2", "47.2.99", false],
    ["47.2.99-beta.10", "47.2.99-beta.2", true],
    ["47.2.99", "unrecognized-build", false],
  ] as const)("orders latest %s against installed %s (offer: %s)", async (latest, current, offer) => {
    info.latest = latest;
    render(<LaunchOverrides instance={{ ...instance, loaderVersion: current }} onLoaderVersionChange={async () => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Check for update" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Check for update" })).toBeEnabled());
    if (offer) {
      expect(screen.getByRole("button", { name: `Update to ${latest}` })).toBeEnabled();
    } else {
      expect(screen.queryByRole("button", { name: /^Update to / })).not.toBeInTheDocument();
      expect(screen.getByText(/No newer build is available/)).toBeInTheDocument();
    }
  });

  it("does not replace missing latest with recommended", async () => {
    info.latest = null;
    info.recommended = "47.2.101";
    render(<LaunchOverrides instance={instance} onLoaderVersionChange={async () => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Check for update" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Check for update" })).toBeEnabled());
    expect(screen.queryByRole("button", { name: /^Update to / })).not.toBeInTheDocument();
  });

  it("does not use recommended-only legacy lookup when latest lookup fails", async () => {
    mockIPC((command) => {
      commands.push(command);
      if (command === "get_loader_version_info") throw new Error("Loader index unavailable");
      if (command === "get_latest_loader_version") return "47.2.0";
      if (command === "get_launch_settings") return { detected: [], javaPath: null, maxMemoryMb: 4096, jvmArgs: null };
      if (command === "get_instance_launch_config") return { javaPath: null, maxMemoryMb: null, jvmArgs: null };
      return null;
    });
    render(<LaunchOverrides instance={instance} onLoaderVersionChange={async () => {}} />);
    fireEvent.click(screen.getByRole("button", { name: "Check for update" }));
    expect(await screen.findByText("Loader index unavailable")).toBeInTheDocument();
    expect(commands).not.toContain("get_latest_loader_version");
    expect(screen.queryByRole("button", { name: /^Update to / })).not.toBeInTheDocument();
  });

  it("ignores a previous instance's late check without clearing the new offer", async () => {
    let finishFirst!: (result: LoaderVersionInfo) => void;
    let checkCount = 0;
    mockIPC((command) => {
      if (command === "get_loader_version_info") {
        if (++checkCount === 1) {
          return new Promise<LoaderVersionInfo>((resolve) => { finishFirst = resolve; });
        }
        return { ...info, latest: "48.2.100", minecraftVersion: "1.20.2" };
      }
      if (command === "get_launch_settings") return { detected: [], javaPath: null, maxMemoryMb: 4096, jvmArgs: null };
      if (command === "get_instance_launch_config") return { javaPath: null, maxMemoryMb: null, jvmArgs: null };
      return null;
    });
    const onLoaderVersionChange = vi.fn(async () => {});
    const view = render(<LaunchOverrides instance={instance} onLoaderVersionChange={onLoaderVersionChange} />);
    fireEvent.click(screen.getByRole("button", { name: "Check for update" }));
    await waitFor(() => expect(checkCount).toBe(1));

    view.rerender(<LaunchOverrides
      instance={{ ...instance, id: "second-instance", minecraftVersion: "1.20.2", loaderVersion: "48.2.99" }}
      onLoaderVersionChange={onLoaderVersionChange}
    />);
    fireEvent.click(screen.getByRole("button", { name: "Check for update" }));
    expect(await screen.findByRole("button", { name: "Update to 48.2.100" })).toBeEnabled();

    await act(async () => { finishFirst({ ...info, latest: "99.0.0" }); });
    expect(screen.getByRole("button", { name: "Update to 48.2.100" })).toBeEnabled();
    expect(screen.queryByRole("button", { name: "Update to 99.0.0" })).not.toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Update to 48.2.100" }));
    fireEvent.click(screen.getByRole("button", { name: "Update" }));
    await waitFor(() => expect(onLoaderVersionChange).toHaveBeenCalledWith("48.2.100"));
  });
});
