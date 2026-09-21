import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";
import { ContextMenu } from "./ContextMenu";
import { ConfirmDialog } from "./ConfirmDialog";

function Flow() {
  const [menu, setMenu] = useState(false);
  const [dialog, setDialog] = useState(false);
  return <>
    <button onClick={() => setMenu(true)}>Actions</button>
    {menu && <ContextMenu x={0} y={0} onClose={() => setMenu(false)} items={[
      { label: "Open", onClick: () => {} },
      { label: "Delete", onClick: () => setDialog(true) },
    ]} />}
    {dialog && <ConfirmDialog title="Delete instance?" message="Cannot undo" onCancel={() => setDialog(false)} onConfirm={() => setDialog(false)} />}
  </>;
}

describe("keyboard menu to modal flow", () => {
  it("navigates menu items, traps modal focus and returns to the original trigger", () => {
    render(<Flow />);
    const trigger = screen.getByRole("button", { name: "Actions" });
    trigger.focus();
    fireEvent.click(trigger);
    expect(screen.getByRole("menuitem", { name: "Open" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" });
    const remove = screen.getByRole("menuitem", { name: "Delete" });
    expect(remove).toHaveFocus();
    fireEvent.click(remove);
    const cancel = screen.getByRole("button", { name: "Cancel" });
    expect(cancel).toHaveFocus();
    fireEvent.keyDown(cancel, { key: "Tab", shiftKey: true });
    expect(screen.getByRole("button", { name: "Confirm" })).toHaveFocus();
    fireEvent.keyDown(document.activeElement!, { key: "Tab" });
    expect(cancel).toHaveFocus();
    fireEvent.keyDown(cancel, { key: "Escape" });
    expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument();
    expect(trigger).toHaveFocus();
  });
});
