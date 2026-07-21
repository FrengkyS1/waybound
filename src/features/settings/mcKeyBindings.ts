// Translate between Minecraft key codes (e.g. "key.keyboard.w", "key.mouse.left")
// and human-readable labels, plus map a browser input event to a Minecraft code.

export const UNBOUND = "key.keyboard.unknown";

const KEYBOARD_LABELS: Record<string, string> = {
  space: "Space",
  "left.shift": "Left Shift",
  "right.shift": "Right Shift",
  "left.control": "Left Ctrl",
  "right.control": "Right Ctrl",
  "left.alt": "Left Alt",
  "right.alt": "Right Alt",
  tab: "Tab",
  "caps.lock": "Caps Lock",
  enter: "Enter",
  backspace: "Backspace",
  delete: "Delete",
  insert: "Insert",
  home: "Home",
  end: "End",
  "page.up": "Page Up",
  "page.down": "Page Down",
  slash: "/",
  period: ".",
  comma: ",",
  semicolon: ";",
  apostrophe: "'",
  "left.bracket": "[",
  "right.bracket": "]",
  backslash: "\\",
  minus: "-",
  equal: "=",
  grave: "`",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
};

/** Human label for a Minecraft key code. */
export function keyLabel(code: string): string {
  if (!code || code === UNBOUND) return "Not bound";
  if (code.startsWith("key.mouse.")) {
    const button = code.slice("key.mouse.".length);
    const named: Record<string, string> = {
      left: "Left Mouse",
      right: "Right Mouse",
      middle: "Middle Mouse",
    };
    return named[button] ?? `Mouse ${button}`;
  }
  if (code.startsWith("key.keyboard.")) {
    const k = code.slice("key.keyboard.".length);
    if (KEYBOARD_LABELS[k]) return KEYBOARD_LABELS[k];
    if (/^f\d+$/.test(k)) return k.toUpperCase();
    if (/^keypad\./.test(k)) return `Num ${k.slice("keypad.".length)}`;
    if (k.length === 1) return k.toUpperCase();
    return k
      .split(".")
      .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
      .join(" ");
  }
  return code;
}

const CODE_MAP: Record<string, string> = {
  Space: "space",
  ShiftLeft: "left.shift",
  ShiftRight: "right.shift",
  ControlLeft: "left.control",
  ControlRight: "right.control",
  AltLeft: "left.alt",
  AltRight: "right.alt",
  Tab: "tab",
  CapsLock: "caps.lock",
  Enter: "enter",
  Backspace: "backspace",
  Delete: "delete",
  Insert: "insert",
  Home: "home",
  End: "end",
  PageUp: "page.up",
  PageDown: "page.down",
  Slash: "slash",
  Period: "period",
  Comma: "comma",
  Semicolon: "semicolon",
  Quote: "apostrophe",
  BracketLeft: "left.bracket",
  BracketRight: "right.bracket",
  Backslash: "backslash",
  Minus: "minus",
  Equal: "equal",
  Backquote: "grave",
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
};

/** Map a KeyboardEvent to a Minecraft key code, or null if unsupported. */
export function codeFromKeyboardEvent(e: KeyboardEvent): string | null {
  const c = e.code;
  if (/^Key[A-Z]$/.test(c)) return `key.keyboard.${c.slice(3).toLowerCase()}`;
  if (/^Digit[0-9]$/.test(c)) return `key.keyboard.${c.slice(5)}`;
  if (/^Numpad[0-9]$/.test(c)) return `key.keyboard.keypad.${c.slice(6)}`;
  if (/^F\d{1,2}$/.test(c)) return `key.keyboard.${c.toLowerCase()}`;
  if (CODE_MAP[c]) return `key.keyboard.${CODE_MAP[c]}`;
  return null;
}

/** Map a mouse button index to a Minecraft key code. */
export function codeFromMouseButton(button: number): string {
  const named: Record<number, string> = { 0: "left", 1: "middle", 2: "right" };
  return `key.mouse.${named[button] ?? button}`;
}
