import { describe, expect, it } from "vitest";

import {
  DEFAULT_KEY_BINDINGS,
  KEY_BINDING_LABELS,
  defaultMcOptions,
  normalizeMcOptions,
} from "./mcOptionsDefaults";
import type { McOptions } from "./types";

describe("defaultMcOptions", () => {
  it("returns the vanilla-ish defaults the settings form starts from", () => {
    const o = defaultMcOptions();
    expect(o.guiScale).toBe(2);
    expect(o.renderDistance).toBe(12);
    expect(o.simulationDistance).toBe(12);
    expect(o.fov).toBe(70);
    expect(o.maxFps).toBe(260);
    expect(o.graphicsMode).toBe("fancy");
    expect(o.narrator).toBe("off");
    expect(o.language).toBe("en_us");
    expect(o.customize).toBe(true);
    expect(o.fullscreen).toBe(false);
  });

  it("hands out a fresh keyBindings object each call", () => {
    const a = defaultMcOptions();
    const b = defaultMcOptions();

    expect(a.keyBindings).toEqual(DEFAULT_KEY_BINDINGS);
    expect(a.keyBindings).not.toBe(b.keyBindings);
    expect(a.keyBindings).not.toBe(DEFAULT_KEY_BINDINGS);

    // Editing one instance must not leak into the shared constant or the
    // next call — the settings form mutates its own copy freely.
    a.keyBindings.jump = "key.keyboard.z";
    expect(DEFAULT_KEY_BINDINGS.jump).toBe("key.keyboard.space");
    expect(defaultMcOptions().keyBindings.jump).toBe("key.keyboard.space");
  });

  it("has a human label for every default binding", () => {
    for (const action of Object.keys(DEFAULT_KEY_BINDINGS)) {
      expect(KEY_BINDING_LABELS[action], `missing label for ${action}`).toBeTruthy();
    }
  });
});

describe("normalizeMcOptions", () => {
  it("returns the full defaults for an empty partial", () => {
    expect(normalizeMcOptions({})).toEqual(defaultMcOptions());
  });

  it("fills every unspecified field from the defaults", () => {
    const result = normalizeMcOptions({ fov: 110, renderDistance: 32 });

    expect(result.fov).toBe(110);
    expect(result.renderDistance).toBe(32);
    // Untouched fields keep default values rather than becoming undefined.
    expect(result.guiScale).toBe(2);
    expect(result.maxFps).toBe(260);
    expect(result.language).toBe("en_us");
    expect(Object.keys(result).sort()).toEqual(Object.keys(defaultMcOptions()).sort());
  });

  it("does not clamp or validate out-of-range numbers", () => {
    // Documented behaviour, not an oversight to paper over in a test: the
    // options file is round-tripped verbatim, so a value the user (or a
    // hand-edited options.txt) put out of range survives normalization and
    // is the UI control's problem, not this function's.
    const result = normalizeMcOptions({
      fov: -999,
      renderDistance: 1_000_000,
      mouseSensitivity: Number.NaN,
      guiScale: 0,
      masterVolume: 250,
    });

    expect(result.fov).toBe(-999);
    expect(result.renderDistance).toBe(1_000_000);
    expect(result.mouseSensitivity).toBeNaN();
    expect(result.guiScale).toBe(0);
    expect(result.masterVolume).toBe(250);
  });

  it("does not validate enum-ish string fields either", () => {
    const result = normalizeMcOptions({
      graphicsMode: "nonsense" as McOptions["graphicsMode"],
      narrator: "" as McOptions["narrator"],
    });

    expect(result.graphicsMode).toBe("nonsense");
    expect(result.narrator).toBe("");
  });

  it("carries unknown keys through instead of dropping them", () => {
    const result = normalizeMcOptions({
      fov: 90,
      someFutureOption: "keep me",
    } as unknown as Partial<McOptions>);

    expect((result as unknown as Record<string, unknown>).someFutureOption).toBe("keep me");
    expect(result.fov).toBe(90);
  });

  it("lets an explicit undefined blank out a scalar default", () => {
    // Spread semantics: `{ ...defaults, ...{ fov: undefined } }` really does
    // set fov to undefined. Callers must omit a key rather than pass
    // undefined for it; pinning this so the day it changes is deliberate.
    const result = normalizeMcOptions({ fov: undefined } as Partial<McOptions>);
    expect(result.fov).toBeUndefined();
  });

  it("merges keyBindings per-key instead of replacing the map", () => {
    const result = normalizeMcOptions({
      keyBindings: { jump: "key.keyboard.z", inventory: "key.mouse.4" },
    });

    expect(result.keyBindings.jump).toBe("key.keyboard.z");
    expect(result.keyBindings.inventory).toBe("key.mouse.4");
    // Everything the partial didn't mention still has its default binding.
    expect(result.keyBindings.forward).toBe("key.keyboard.w");
    expect(result.keyBindings["hotbar.9"]).toBe("key.keyboard.9");
    expect(Object.keys(result.keyBindings).sort()).toEqual(
      Object.keys(DEFAULT_KEY_BINDINGS).sort(),
    );
  });

  it("keeps bindings for actions the defaults have never heard of", () => {
    const result = normalizeMcOptions({
      keyBindings: { "key.somemod.dothing": "key.keyboard.g" },
    });

    expect(result.keyBindings["key.somemod.dothing"]).toBe("key.keyboard.g");
    expect(result.keyBindings.forward).toBe("key.keyboard.w");
  });

  it("survives a missing keyBindings map", () => {
    const result = normalizeMcOptions({ fov: 80 });
    expect(result.keyBindings).toEqual(DEFAULT_KEY_BINDINGS);
    // Still a copy, so later edits can't corrupt the shared constant.
    expect(result.keyBindings).not.toBe(DEFAULT_KEY_BINDINGS);
  });

  it("does not mutate the partial it was given", () => {
    const partial: Partial<McOptions> = {
      fov: 100,
      keyBindings: { jump: "key.keyboard.z" },
    };
    const snapshot = JSON.parse(JSON.stringify(partial)) as unknown;

    normalizeMcOptions(partial);

    expect(JSON.parse(JSON.stringify(partial))).toEqual(snapshot);
  });
});
