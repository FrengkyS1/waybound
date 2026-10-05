import { fireEvent, render, screen } from "@testing-library/react";
import { useState } from "react";
import { describe, expect, it } from "vitest";

import { McOptionsForm } from "./McOptionsForm";
import { defaultMcOptions } from "./mcOptionsDefaults";

function OptionsForm() {
  const [options, setOptions] = useState(defaultMcOptions);
  return <McOptionsForm options={options} disabled={false} onChange={(partial) => {
    setOptions((current) => ({ ...current, ...partial }));
  }} />;
}

describe("Minecraft settings controls", () => {
  it("names bindings by action and current key, including listening state", () => {
    render(<OptionsForm />);
    expect(screen.getByRole("button", { name: "Walk forward: W" })).toBeEnabled();
    expect(screen.getByRole("button", { name: "Attack / destroy: Left Mouse" })).toBeEnabled();

    const jump = screen.getByRole("button", { name: "Jump: Space" });
    jump.focus();
    fireEvent.click(jump);
    expect(screen.getByRole("button", {
      name: "Jump: Space; listening, press a key or mouse button; Escape to unbind",
    })).toHaveAttribute("aria-pressed", "true");
    fireEvent.keyDown(window, { key: "z", code: "KeyZ" });
    expect(screen.getByRole("button", { name: "Jump: Z" })).toHaveAttribute("aria-pressed", "false");

    fireEvent.click(screen.getByRole("button", { name: "Jump: Z" }));
    fireEvent.keyDown(window, { key: "Escape", code: "Escape" });
    expect(screen.getByRole("button", { name: "Jump: Not bound" })).toBeEnabled();
  });

  it("keeps one-degree FOV and odd sensitivity input steps", () => {
    render(<OptionsForm />);
    const fov = screen.getByRole("slider", { name: /^FOV\s*\d+$/ });
    const sensitivity = screen.getByRole("slider", { name: /^Mouse sensitivity\s*\d+$/ });
    fireEvent.change(fov, { target: { value: "71" } });
    fireEvent.change(sensitivity, { target: { value: "51" } });
    expect(fov).toHaveValue("71");
    expect(sensitivity).toHaveValue("51");
  });
});
