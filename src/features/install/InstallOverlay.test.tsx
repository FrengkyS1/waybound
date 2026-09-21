import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen } from "@testing-library/react";
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
