import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

/**
 * Same module-scope constraint as `installStore.test.ts`: `installStore`
 * wires `listen(...)` and restores pending missing mods on import, so both
 * modules load dynamically after the IPC mock exists, with a fresh copy
 * per test.
 */
type OverlayModule = typeof import("./InstallOverlay");
type StoreModule = typeof import("./installStore");

let InstallOverlay: OverlayModule["InstallOverlay"];
let useInstallStore: StoreModule["useInstallStore"];

beforeEach(async () => {
  vi.resetModules();
  mockIPC((cmd) => {
    if (cmd === "list_pending_missing_mods") return [];
    return undefined;
  });
  ({ InstallOverlay } = await import("./InstallOverlay"));
  ({ useInstallStore } = await import("./installStore"));
  useInstallStore.setState({ installs: [], notifications: [], dockMinimized: false });
  await waitFor(() => expect(useInstallStore.getState().pendingMissingModsLoading).toBe(false));
});

function seedInstalling() {
  useInstallStore.setState({
    installs: [{ id: "e1", name: "Some Pack", status: "installing" }],
  });
}

describe("dock minimize", () => {
  it("renders nothing when there is nothing to show", () => {
    const { container } = render(<InstallOverlay />);
    expect(container).toBeEmptyDOMElement();
  });

  it("collapses to a peek tab and restores on click", () => {
    seedInstalling();
    render(<InstallOverlay />);

    expect(screen.getByLabelText("Minimize notifications")).toBeInTheDocument();

    fireEvent.click(screen.getByLabelText("Minimize notifications"));
    expect(screen.queryByText("Some Pack")).not.toBeInTheDocument();
    const peek = screen.getByLabelText("Show 1 notification");
    expect(peek).toBeInTheDocument();

    fireEvent.click(peek);
    expect(screen.getByText("Some Pack")).toBeInTheDocument();
    expect(screen.getByLabelText("Minimize notifications")).toBeInTheDocument();
  });
});

describe("manual download recovery controls", () => {
  it("keeps pending manual downloads actionable on failed install cards", () => {
    useInstallStore.setState({ installs: [{
      id: "manual", name: "Failed update", status: "error", error: "Could not finish update", instanceId: "isolated",
      missingMods: [{ projectId: 1, name: "Restricted", filename: "restricted.jar", url: "https://www.curseforge.com/minecraft/mc-mods/restricted/download/1" }],
    }] });
    render(<InstallOverlay />);
    expect(screen.getByRole("button", { name: "Download missing mods (1)" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Not installing this" })).toBeEnabled();
  });

  it("clears browser and watcher errors independently after successful retries", async () => {
    useInstallStore.setState({
      installs: [{
        id: "manual", name: "Restricted pack", status: "done", instanceId: "isolated", missingModsOpenAll: true,
        missingMods: [{ projectId: 1, name: "Restricted", filename: "restricted.jar", url: "https://www.curseforge.com/minecraft/mc-mods/restricted" }],
        missingModsBrowserError: "Browser could not open", missingModsWatchError: "Downloads unreadable",
      }],
    });
    render(<InstallOverlay />);
    expect(screen.getByText("Browser could not open")).toHaveAttribute("role", "alert");
    expect(screen.getByText("Downloads unreadable")).toHaveAttribute("role", "alert");
    fireEvent.click(screen.getByRole("button", { name: "Retry open all" }));
    await waitFor(() => expect(screen.queryByText("Browser could not open")).not.toBeInTheDocument());
    expect(screen.getByText("Downloads unreadable")).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Retry watching Downloads" }));
    await waitFor(() => expect(screen.queryByText("Downloads unreadable")).not.toBeInTheDocument());
    expect(screen.getByText("Watching Downloads — downloaded files are placed automatically.")).toBeInTheDocument();
  });

  it("clears pending-list errors after recovery succeeds without cards", async () => {
    useInstallStore.setState({ pendingMissingModsError: "Could not restore pending manual downloads" });
    const { container } = render(<InstallOverlay />);
    expect(screen.getByRole("alert")).toHaveTextContent("Could not restore pending manual downloads");
    fireEvent.click(screen.getByRole("button", { name: "Retry manual downloads" }));
    await waitFor(() => expect(container).toBeEmptyDOMElement());
  });
});
