import { mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ConfigEditorModal } from "./ConfigEditorModal";

const files = [
  { relativePath: "config/first.toml", displayName: "first.toml" },
  { relativePath: "config/second.toml", displayName: "second.toml" },
];

describe("config file identity", () => {
  it("ignores a late read after switching to another file", async () => {
    let finishFirst!: (value: string) => void;
    mockIPC((command, args) => {
      if (command === "list_mod_configs") return files;
      if (command === "read_config_file") return args !== null && typeof args === "object" && "relativePath" in args && args.relativePath === files[0].relativePath
        ? new Promise<string>((resolve) => { finishFirst = resolve; }) : "second = true";
      return null;
    });
    render(<ConfigEditorModal instanceId="isolated" fileName="sample.jar" modLabel="Sample" onClose={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "first.toml" }));
    fireEvent.click(screen.getByRole("button", { name: "second.toml" }));
    expect(await screen.findByRole("textbox")).toHaveValue("second = true");
    await act(async () => { finishFirst("first = true"); });
    expect(screen.getByRole("textbox")).toHaveValue("second = true");
    expect(screen.getByRole("textbox")).toHaveAccessibleName("Editing second.toml");
  });

  it("keeps the save bound to its file until the write completes", async () => {
    let finishSave!: () => void;
    const close = vi.fn();
    const writes: unknown[] = [];
    mockIPC((command, args) => {
      if (command === "list_mod_configs") return files;
      if (command === "read_config_file") return "enabled = true";
      if (command === "write_config_file") {
        writes.push(args);
        return new Promise<void>((resolve) => { finishSave = resolve; });
      }
      return null;
    });
    render(<ConfigEditorModal instanceId="isolated" fileName="sample.jar" modLabel="Sample" onClose={close} />);
    fireEvent.click(await screen.findByRole("button", { name: "first.toml" }));
    fireEvent.change(await screen.findByRole("textbox"), { target: { value: "enabled = false" } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(screen.getByRole("button", { name: "second.toml" })).toBeDisabled();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(close).not.toHaveBeenCalled();
    await act(async () => { finishSave(); });
    await waitFor(() => expect(screen.getByRole("button", { name: "second.toml" })).toBeEnabled());
    expect(writes).toEqual([{ instanceId: "isolated", relativePath: files[0].relativePath, contents: "enabled = false" }]);
    fireEvent.click(screen.getByRole("button", { name: "second.toml" }));
    expect(await screen.findByRole("textbox", { name: "Editing second.toml" })).toHaveValue("enabled = true");
  });
});
