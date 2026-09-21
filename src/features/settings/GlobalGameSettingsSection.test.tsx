import { mockIPC } from "@tauri-apps/api/mocks";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { GlobalGameSettingsSection } from "./GlobalGameSettingsSection";
import { normalizeMcOptions } from "./mcOptionsDefaults";

describe("global customization persistence", () => {
  it("allows saving Customize OFF and restores it on a later visit", async () => {
    let options = { ...normalizeMcOptions({}), customize: true };
    mockIPC((command, args) => {
      if (command === "save_global_mc_options" && args !== null && typeof args === "object" && "options" in args) options = args.options as typeof options;
      return { options, configured: true, applyToNewInstances: true };
    });
    const first = render(<GlobalGameSettingsSection />);
    fireEvent.click(await screen.findByRole("switch", { name: "Customize game settings" }));
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(options.customize).toBe(false));
    first.unmount();
    render(<GlobalGameSettingsSection />);
    expect(await screen.findByRole("switch", { name: "Customize game settings" })).toHaveAttribute("aria-checked", "false");
  });
});
