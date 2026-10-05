import { mockIPC } from "@tauri-apps/api/mocks";
import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { ConfigEditorModal } from "./ConfigEditorModal";

const files = [
  { relativePath: "first.toml", displayName: "first.toml" },
  { relativePath: "second.toml", displayName: "second.toml" },
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
    render(<ConfigEditorModal instanceId="isolated" scope="mod" fileName="sample.jar" title="Sample" emptyHint="No configs." onClose={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "first.toml" }));
    fireEvent.click(screen.getByRole("button", { name: "second.toml" }));
    expect(await screen.findByRole("textbox", { name: "Editing second.toml" })).toHaveValue("second = true");
    await act(async () => { finishFirst("first = true"); });
    expect(screen.getByRole("textbox", { name: "Editing second.toml" })).toHaveValue("second = true");
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
    render(<ConfigEditorModal instanceId="isolated" scope="mod" fileName="sample.jar" title="Sample" emptyHint="No configs." onClose={close} />);
    fireEvent.click(await screen.findByRole("button", { name: "first.toml" }));
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" });
    act(() => editor.focus());
    fireEvent.change(editor, { target: { value: "enabled = false" } });
    fireEvent.keyDown(editor, { key: "s", ctrlKey: true });
    expect(editor).toHaveFocus();
    expect(editor).toBeEnabled();
    expect(editor).toHaveAttribute("readonly");
    fireEvent.keyDown(editor, { key: "Tab" });
    expect(editor).toHaveValue("enabled = false");
    act(() => editor.focus());
    expect(screen.getByRole("button", { name: "second.toml" })).toBeDisabled();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(close).not.toHaveBeenCalled();
    await act(async () => { finishSave(); });
    await waitFor(() => expect(screen.getByRole("button", { name: "second.toml" })).toBeEnabled());
    expect(editor).toHaveFocus();
    expect(editor).not.toHaveAttribute("readonly");
    expect(writes).toEqual([{ instanceId: "isolated", relativePath: files[0].relativePath, contents: "enabled = false", expectedContents: "enabled = true" }]);
    fireEvent.click(screen.getByRole("button", { name: "second.toml" }));
    expect(await screen.findByRole("textbox", { name: "Editing second.toml" })).toHaveValue("enabled = true");
  });

  it("lists and saves through the world commands in world scope", async () => {
    const writes: unknown[] = [];
    mockIPC((command, args) => {
      if (command === "list_world_files") {
        expect(args).toEqual({ instanceId: "isolated", worldFolder: "My World" });
        return [
          { relativePath: "stats/uuid.json", displayName: "stats/uuid.json" },
          { relativePath: "advancements/done.json", displayName: "advancements/done.json" },
        ];
      }
      if (command === "read_world_file") return "{}";
      if (command === "write_world_file") {
        writes.push(args);
        return null;
      }
      return null;
    });
    render(
      <ConfigEditorModal
        instanceId="isolated"
        scope="world"
        worldFolder="My World"
        title="My World"
        emptyHint="Nothing editable."
        onClose={() => {}}
      />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "stats/uuid.json" }));
    fireEvent.change(await screen.findByRole("textbox", { name: "Editing stats/uuid.json" }), { target: { value: '{"x":1}' } });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(writes).toHaveLength(1));
    expect(writes).toEqual([
      { instanceId: "isolated", worldFolder: "My World", relativePath: "stats/uuid.json", contents: '{"x":1}', expectedContents: "{}" },
    ]);
  });

  it("keeps failed-save drafts editable and retries with the same disk baseline", async () => {
    const writes: unknown[] = [];
    mockIPC((command, args) => {
      if (command === "list_instance_configs") return [files[0]];
      if (command === "read_config_file") return "enabled = true";
      if (command === "write_config_file") {
        writes.push(args);
        if (writes.length === 1) return Promise.reject("Permission denied");
      }
      return null;
    });
    render(<ConfigEditorModal instanceId="isolated" scope="instance" title="Configs" emptyHint="No configs." onClose={() => {}} />);
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" });
    fireEvent.change(editor, { target: { value: "enabled = false" } });
    fireEvent.keyDown(editor, { key: "s", ctrlKey: true });
    expect(await screen.findByRole("alert")).toHaveTextContent("Permission denied");
    expect(editor).toHaveValue("enabled = false");
    expect(editor).toBeEnabled();
    fireEvent.change(editor, { target: { value: "enabled = false\nextra = true" } });
    fireEvent.click(screen.getByRole("button", { name: "Retry Save" }));
    await waitFor(() => expect(screen.getByRole("button", { name: "Save" })).toBeDisabled());
    expect(writes).toEqual([
      { instanceId: "isolated", relativePath: files[0].relativePath, contents: "enabled = false", expectedContents: "enabled = true" },
      { instanceId: "isolated", relativePath: files[0].relativePath, contents: "enabled = false\nextra = true", expectedContents: "enabled = true" },
    ]);
  });

  it("preserves an external edit and guards reload of the conflicting draft", async () => {
    let disk = "original";
    const reads = vi.fn(() => disk);
    mockIPC((command, args) => {
      if (command === "list_instance_configs") return [files[0]];
      if (command === "read_config_file") return reads();
      if (command === "write_config_file") {
        const write = args as { expectedContents: string; contents: string };
        if (write.expectedContents !== disk) return Promise.reject("This file changed on disk.");
        disk = write.contents;
      }
      return null;
    });
    render(<ConfigEditorModal instanceId="isolated" scope="instance" title="Configs" emptyHint="No configs." onClose={() => {}} />);
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" });
    fireEvent.change(editor, { target: { value: "my draft" } });
    disk = "external change";
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("changed on disk");
    expect(editor).toHaveValue("my draft");
    expect(disk).toBe("external change");
    fireEvent.click(screen.getByRole("button", { name: "Reload from disk" }));
    expect(reads).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("alertdialog", { name: "Reload and keep draft?" })).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    await waitFor(() => expect(screen.getByRole("textbox", { name: "Editing first.toml" })).toHaveValue("external change"));
    const restore = screen.getByRole("button", { name: "Restore previous draft" });
    act(() => restore.focus());
    fireEvent.click(restore);
    expect(screen.getByRole("textbox", { name: "Editing first.toml" })).toHaveValue("my draft");
    expect(disk).toBe("external change");
    await waitFor(() => expect(editor).toHaveFocus());
    fireEvent.keyDown(document.activeElement!, { key: "s", ctrlKey: true });
    await waitFor(() => expect(disk).toBe("my draft"));
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("filters file paths without losing a draft, finds literal matches and toggles case", async () => {
    mockIPC((command) => {
      if (command === "list_mod_configs") return files;
      if (command === "read_config_file") return "Alpha alpha ALPHA\n[a]";
      return null;
    });
    render(<ConfigEditorModal instanceId="isolated" scope="mod" fileName="sample.jar" title="Sample" emptyHint="No configs." onClose={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "first.toml" }));
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" }) as HTMLTextAreaElement;
    fireEvent.change(editor, { target: { value: "Alpha alpha ALPHA\n[a]\ndraft" } });
    fireEvent.change(screen.getByRole("textbox", { name: "Filter files" }), { target: { value: "second" } });
    expect(screen.queryByRole("button", { name: "first.toml" })).not.toBeInTheDocument();
    expect(editor).toHaveValue("Alpha alpha ALPHA\n[a]\ndraft");
    fireEvent.keyDown(editor, { key: "f", ctrlKey: true });
    const find = screen.getByRole("textbox", { name: "Find in file" });
    expect(find).toHaveFocus();
    fireEvent.change(find, { target: { value: "alpha" } });
    expect(screen.getByText("1 of 3")).toBeVisible();
    fireEvent.click(screen.getByRole("button", { name: "Next match" }));
    expect(editor.selectionStart).toBe(6);
    expect(editor.selectionEnd).toBe(11);
    fireEvent.click(screen.getByRole("button", { name: "Previous match" }));
    expect(editor.selectionStart).toBe(0);
    fireEvent.click(screen.getByRole("button", { name: "Match case" }));
    expect(screen.getByText("1 of 1")).toBeVisible();
    fireEvent.change(find, { target: { value: "[a]" } });
    fireEvent.keyDown(find, { key: "Enter" });
    expect(editor.selectionStart).toBe(18);
    expect(screen.getByText("Ln 2, Col 1")).toBeVisible();
    fireEvent.change(find, { target: { value: "missing" } });
    expect(screen.getByText("0 of 0")).toBeVisible();
    expect(screen.getByRole("button", { name: "Next match" })).toBeDisabled();
  });

  it("keeps the draft and its original save baseline when a guarded reload fails", async () => {
    let failRead = false;
    const writes: unknown[] = [];
    mockIPC((command, args) => {
      if (command === "list_instance_configs") return [files[0]];
      if (command === "read_config_file") return failRead ? Promise.reject("File locked") : "original";
      if (command === "write_config_file") writes.push(args);
      return null;
    });
    render(<ConfigEditorModal instanceId="isolated" scope="instance" title="Configs" emptyHint="No configs." onClose={() => {}} />);
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" });
    fireEvent.change(editor, { target: { value: "draft" } });
    failRead = true;
    fireEvent.click(screen.getByRole("button", { name: "Reload from disk" }));
    fireEvent.click(screen.getByRole("button", { name: "Reload" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("File locked");
    expect(editor).toHaveValue("draft");
    expect(editor).toBeEnabled();
    fireEvent.keyDown(editor, { key: "s", ctrlKey: true });
    await waitFor(() => expect(writes).toEqual([
      { instanceId: "isolated", relativePath: "first.toml", contents: "draft", expectedContents: "original" },
    ]));
  });

  it("cancels dirty reload, retries an initial read, and wraps keyboard find matches", async () => {
    let failRead = true;
    mockIPC((command) => {
      if (command === "list_mod_configs") return [files[0]];
      if (command === "read_config_file") return failRead ? Promise.reject("Read denied") : "one one";
      return null;
    });
    render(<ConfigEditorModal instanceId="isolated" scope="mod" fileName="sample.jar" title="Sample" emptyHint="No configs." onClose={() => {}} />);
    expect(await screen.findByRole("alert")).toHaveTextContent("Read denied");
    failRead = false;
    fireEvent.click(screen.getByRole("button", { name: "Retry read" }));
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" }) as HTMLTextAreaElement;
    fireEvent.change(editor, { target: { value: "one one draft" } });
    fireEvent.click(screen.getByRole("button", { name: "Reload from disk" }));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(editor).toHaveValue("one one draft");
    fireEvent.keyDown(editor, { key: "f", ctrlKey: true });
    const find = screen.getByRole("textbox", { name: "Find in file" });
    fireEvent.change(find, { target: { value: "one" } });
    fireEvent.keyDown(find, { key: "Enter", shiftKey: true });
    expect(editor.selectionStart).toBe(4);
    expect(screen.getByText("2 of 2")).toBeVisible();
    fireEvent.keyDown(editor, { key: "F3" });
    expect(editor.selectionStart).toBe(0);
    fireEvent.keyDown(editor, { key: "m", ctrlKey: true });
    act(() => editor.focus());
    const tab = new KeyboardEvent("keydown", { key: "Tab", bubbles: true, cancelable: true });
    fireEvent(editor, tab);
    expect(editor).toHaveValue("one one draft");
    fireEvent.keyDown(editor, { key: "m", ctrlKey: true });
    editor.setSelectionRange(0, 0);
    fireEvent.keyDown(editor, { key: "Tab" });
    expect(editor).toHaveValue("  one one draft");
  });

  it("indents and unindents a multiline selection with keyboard shortcuts", async () => {
    mockIPC((command) => command === "list_mod_configs" ? [files[0]] : command === "read_config_file" ? "one\ntwo" : null);
    render(<ConfigEditorModal instanceId="isolated" scope="mod" fileName="sample.jar" title="Sample" emptyHint="No configs." onClose={() => {}} />);
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" }) as HTMLTextAreaElement;
    editor.setSelectionRange(0, 7);
    fireEvent.keyDown(editor, { key: "Tab" });
    expect(editor).toHaveValue("  one\n  two");
    await waitFor(() => {
      expect(editor.selectionStart).toBe(2);
      expect(editor.selectionEnd).toBe(11);
    });
    fireEvent.keyDown(editor, { key: "Tab", shiftKey: true });
    expect(editor).toHaveValue("one\ntwo");
    await waitFor(() => expect(editor.selectionEnd).toBe(7));
    fireEvent.change(editor, { target: { value: "  one\ntwo" } });
    editor.setSelectionRange(0, 0);
    fireEvent.keyDown(editor, { key: "Tab", shiftKey: true });
    expect(editor).toHaveValue("one\ntwo");
  });


  it("selects the first query/case match without stealing find focus or later edit caret", async () => {
    mockIPC((command) => command === "list_instance_configs" ? [files[0]]
      : command === "read_config_file" ? "prefix needle and NEEDLE" : null);
    render(<ConfigEditorModal instanceId="isolated" scope="instance" title="Configs" emptyHint="No configs." onClose={() => {}} />);
    const editor = await screen.findByRole("textbox", { name: "Editing first.toml" }) as HTMLTextAreaElement;
    fireEvent.keyDown(editor, { key: "f", ctrlKey: true });
    const find = screen.getByRole("textbox", { name: "Find in file" });
    fireEvent.change(find, { target: { value: "needle" } });
    expect(find).toHaveFocus();
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([7, 13]);
    fireEvent.click(screen.getByRole("button", { name: "Next match" }));
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([18, 24]);
    act(() => find.focus());
    fireEvent.change(find, { target: { value: "NEEDLE" } });
    expect(find).toHaveFocus();
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([7, 13]);
    const caseButton = screen.getByRole("button", { name: "Match case" });
    act(() => caseButton.focus());
    fireEvent.click(caseButton);
    expect(caseButton).toHaveFocus();
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([18, 24]);
    act(() => editor.focus());
    fireEvent.change(editor, { target: { value: "x prefix needle and NEEDLE", selectionStart: 3, selectionEnd: 3 } });
    expect(editor).toHaveFocus();
    expect([editor.selectionStart, editor.selectionEnd]).toEqual([3, 3]);
  });
});
