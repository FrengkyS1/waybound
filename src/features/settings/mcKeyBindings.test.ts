import { describe, expect, it } from "vitest";

import { UNBOUND, codeFromKeyboardEvent, codeFromMouseButton, keyLabel } from "./mcKeyBindings";
import { DEFAULT_KEY_BINDINGS } from "./mcOptionsDefaults";

function keyEvent(code: string): KeyboardEvent {
  return new KeyboardEvent("keydown", { code });
}

describe("UNBOUND", () => {
  it("is the code Minecraft itself writes for an unbound action", () => {
    expect(UNBOUND).toBe("key.keyboard.unknown");
    // Two vanilla defaults ship unbound; if this constant drifts they'd
    // start rendering as the literal word "Unknown".
    expect(DEFAULT_KEY_BINDINGS.smoothCamera).toBe(UNBOUND);
    expect(DEFAULT_KEY_BINDINGS.spectatorOutlines).toBe(UNBOUND);
  });
});

describe("keyLabel", () => {
  it("reports unbound for the unbound code and for an empty code", () => {
    expect(keyLabel(UNBOUND)).toBe("Not bound");
    expect(keyLabel("")).toBe("Not bound");
  });

  it("names the three mouse buttons Minecraft names", () => {
    expect(keyLabel("key.mouse.left")).toBe("Left Mouse");
    expect(keyLabel("key.mouse.right")).toBe("Right Mouse");
    expect(keyLabel("key.mouse.middle")).toBe("Middle Mouse");
  });

  it("falls back to a numbered label for extra mouse buttons", () => {
    expect(keyLabel("key.mouse.4")).toBe("Mouse 4");
    expect(keyLabel("key.mouse.11")).toBe("Mouse 11");
  });

  it("uses the named-key table before any other rule", () => {
    expect(keyLabel("key.keyboard.space")).toBe("Space");
    expect(keyLabel("key.keyboard.left.shift")).toBe("Left Shift");
    expect(keyLabel("key.keyboard.right.control")).toBe("Right Ctrl");
    expect(keyLabel("key.keyboard.caps.lock")).toBe("Caps Lock");
    expect(keyLabel("key.keyboard.page.down")).toBe("Page Down");
  });

  it("renders punctuation keys as the glyph, not the word", () => {
    expect(keyLabel("key.keyboard.slash")).toBe("/");
    expect(keyLabel("key.keyboard.grave")).toBe("`");
    expect(keyLabel("key.keyboard.backslash")).toBe("\\");
    expect(keyLabel("key.keyboard.apostrophe")).toBe("'");
    expect(keyLabel("key.keyboard.left.bracket")).toBe("[");
  });

  it("renders arrows as arrow glyphs, not as the mouse-ish words", () => {
    expect(keyLabel("key.keyboard.up")).toBe("↑");
    expect(keyLabel("key.keyboard.down")).toBe("↓");
    // "left"/"right" collide with the mouse button names; the keyboard
    // prefix has to win here.
    expect(keyLabel("key.keyboard.left")).toBe("←");
    expect(keyLabel("key.keyboard.right")).toBe("→");
  });

  it("upper-cases function keys", () => {
    expect(keyLabel("key.keyboard.f2")).toBe("F2");
    expect(keyLabel("key.keyboard.f11")).toBe("F11");
  });

  it("prefixes numpad keys with Num", () => {
    expect(keyLabel("key.keyboard.keypad.5")).toBe("Num 5");
    expect(keyLabel("key.keyboard.keypad.add")).toBe("Num add");
  });

  it("upper-cases single-character keys", () => {
    expect(keyLabel("key.keyboard.w")).toBe("W");
    expect(keyLabel("key.keyboard.1")).toBe("1");
  });

  it("title-cases anything else it doesn't recognise", () => {
    expect(keyLabel("key.keyboard.print.screen")).toBe("Print Screen");
    expect(keyLabel("key.keyboard.scroll.lock")).toBe("Scroll Lock");
    expect(keyLabel("key.keyboard.wingding")).toBe("Wingding");
  });

  it("passes through a code from a namespace it knows nothing about", () => {
    expect(keyLabel("key.gamepad.a")).toBe("key.gamepad.a");
    expect(keyLabel("scancode.42")).toBe("scancode.42");
  });

  it("produces a readable label for every vanilla default binding", () => {
    for (const [action, code] of Object.entries(DEFAULT_KEY_BINDINGS)) {
      const label = keyLabel(code);
      expect(label, `empty label for ${action}`).not.toBe("");
      // Only the deliberately-unbound defaults may read as "Not bound".
      if (code !== UNBOUND) expect(label, `${action} looks unbound`).not.toBe("Not bound");
      // A raw code leaking through means keyLabel didn't understand it.
      expect(label, `${action} label is a raw code`).not.toContain("key.");
    }
  });
});

describe("codeFromKeyboardEvent", () => {
  it("maps letter keys by physical position, lower-cased", () => {
    expect(codeFromKeyboardEvent(keyEvent("KeyW"))).toBe("key.keyboard.w");
    expect(codeFromKeyboardEvent(keyEvent("KeyZ"))).toBe("key.keyboard.z");
  });

  it("maps the number row without the Digit prefix", () => {
    expect(codeFromKeyboardEvent(keyEvent("Digit0"))).toBe("key.keyboard.0");
    expect(codeFromKeyboardEvent(keyEvent("Digit7"))).toBe("key.keyboard.7");
  });

  it("maps the numpad digits into the keypad namespace", () => {
    expect(codeFromKeyboardEvent(keyEvent("Numpad0"))).toBe("key.keyboard.keypad.0");
    expect(codeFromKeyboardEvent(keyEvent("Numpad9"))).toBe("key.keyboard.keypad.9");
  });

  it("maps function keys, including the two-digit ones", () => {
    expect(codeFromKeyboardEvent(keyEvent("F1"))).toBe("key.keyboard.f1");
    expect(codeFromKeyboardEvent(keyEvent("F12"))).toBe("key.keyboard.f12");
  });

  it("maps named keys through the lookup table", () => {
    expect(codeFromKeyboardEvent(keyEvent("Space"))).toBe("key.keyboard.space");
    expect(codeFromKeyboardEvent(keyEvent("ShiftLeft"))).toBe("key.keyboard.left.shift");
    expect(codeFromKeyboardEvent(keyEvent("ControlRight"))).toBe("key.keyboard.right.control");
    expect(codeFromKeyboardEvent(keyEvent("Quote"))).toBe("key.keyboard.apostrophe");
    expect(codeFromKeyboardEvent(keyEvent("Backquote"))).toBe("key.keyboard.grave");
    expect(codeFromKeyboardEvent(keyEvent("ArrowLeft"))).toBe("key.keyboard.left");
  });

  it("returns null for keys it has no Minecraft code for", () => {
    // Non-digit numpad keys are deliberately not in the digit branch and
    // not in the table either.
    expect(codeFromKeyboardEvent(keyEvent("NumpadAdd"))).toBeNull();
    expect(codeFromKeyboardEvent(keyEvent("NumpadEnter"))).toBeNull();
    expect(codeFromKeyboardEvent(keyEvent("MediaPlayPause"))).toBeNull();
    expect(codeFromKeyboardEvent(keyEvent("MetaLeft"))).toBeNull();
    expect(codeFromKeyboardEvent(keyEvent(""))).toBeNull();
  });

  it("accepts any one- or two-digit function key, even ones past F12", () => {
    // The regex is width-based, not range-based: F13..F24 exist on some
    // keyboards and are passed straight through to Minecraft.
    expect(codeFromKeyboardEvent(keyEvent("F13"))).toBe("key.keyboard.f13");
    expect(codeFromKeyboardEvent(keyEvent("F24"))).toBe("key.keyboard.f24");
    expect(codeFromKeyboardEvent(keyEvent("F123"))).toBeNull();
  });

  it("ignores e.key, keying only off the physical e.code", () => {
    // A Dvorak or AZERTY layout reports a different e.key for the same
    // physical key; Minecraft binds the physical position.
    const e = new KeyboardEvent("keydown", { code: "KeyW", key: "," });
    expect(codeFromKeyboardEvent(e)).toBe("key.keyboard.w");
  });

  it("round-trips every mapped key back into a readable label", () => {
    const codes = [
      "KeyA",
      "Digit3",
      "Numpad4",
      "F5",
      "Space",
      "ShiftLeft",
      "ShiftRight",
      "ControlLeft",
      "AltRight",
      "Tab",
      "CapsLock",
      "Enter",
      "Backspace",
      "Delete",
      "Insert",
      "Home",
      "End",
      "PageUp",
      "PageDown",
      "Slash",
      "Period",
      "Comma",
      "Semicolon",
      "Quote",
      "BracketLeft",
      "BracketRight",
      "Backslash",
      "Minus",
      "Equal",
      "Backquote",
      "ArrowUp",
      "ArrowDown",
      "ArrowLeft",
      "ArrowRight",
    ];

    for (const code of codes) {
      const mc = codeFromKeyboardEvent(keyEvent(code));
      expect(mc, `${code} produced no code`).not.toBeNull();
      const label = keyLabel(mc as string);
      expect(label, `${code} rendered as unbound`).not.toBe("Not bound");
      expect(label, `${code} rendered as a raw code`).not.toContain("key.keyboard.");
    }
  });

  it("agrees with the vanilla defaults for keys that appear in both", () => {
    expect(codeFromKeyboardEvent(keyEvent("KeyW"))).toBe(DEFAULT_KEY_BINDINGS.forward);
    expect(codeFromKeyboardEvent(keyEvent("Space"))).toBe(DEFAULT_KEY_BINDINGS.jump);
    expect(codeFromKeyboardEvent(keyEvent("ShiftLeft"))).toBe(DEFAULT_KEY_BINDINGS.sneak);
    expect(codeFromKeyboardEvent(keyEvent("ControlLeft"))).toBe(DEFAULT_KEY_BINDINGS.sprint);
    expect(codeFromKeyboardEvent(keyEvent("Tab"))).toBe(DEFAULT_KEY_BINDINGS.playerlist);
    expect(codeFromKeyboardEvent(keyEvent("Slash"))).toBe(DEFAULT_KEY_BINDINGS.command);
    expect(codeFromKeyboardEvent(keyEvent("F5"))).toBe(DEFAULT_KEY_BINDINGS.togglePerspective);
    expect(codeFromKeyboardEvent(keyEvent("Digit1"))).toBe(DEFAULT_KEY_BINDINGS["hotbar.1"]);
  });
});

describe("codeFromMouseButton", () => {
  it("uses DOM button order, where 1 is middle and 2 is right", () => {
    expect(codeFromMouseButton(0)).toBe("key.mouse.left");
    expect(codeFromMouseButton(1)).toBe("key.mouse.middle");
    expect(codeFromMouseButton(2)).toBe("key.mouse.right");
  });

  it("passes extra buttons through by index", () => {
    expect(codeFromMouseButton(3)).toBe("key.mouse.3");
    expect(codeFromMouseButton(4)).toBe("key.mouse.4");
  });

  it("round-trips into the labels Minecraft's own defaults use", () => {
    expect(codeFromMouseButton(0)).toBe(DEFAULT_KEY_BINDINGS.attack);
    expect(codeFromMouseButton(2)).toBe(DEFAULT_KEY_BINDINGS.use);
    expect(codeFromMouseButton(1)).toBe(DEFAULT_KEY_BINDINGS.pickItem);

    expect(keyLabel(codeFromMouseButton(0))).toBe("Left Mouse");
    expect(keyLabel(codeFromMouseButton(1))).toBe("Middle Mouse");
    expect(keyLabel(codeFromMouseButton(2))).toBe("Right Mouse");
    expect(keyLabel(codeFromMouseButton(4))).toBe("Mouse 4");
  });
});
