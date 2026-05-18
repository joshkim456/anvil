// Map a browser input event to a Minecraft 1.20.1 keybind token. We key off
// KeyboardEvent.code (physical, layout-independent) so it lines up with
// Minecraft's scancode-style names regardless of keyboard layout.

const CODE_TO_MC: Record<string, string> = {
  Space: "space",
  Enter: "enter",
  Escape: "escape",
  Tab: "tab",
  Backspace: "backspace",
  CapsLock: "caps.lock",
  Delete: "delete",
  Insert: "insert",
  Home: "home",
  End: "end",
  PageUp: "page.up",
  PageDown: "page.down",
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
  ShiftLeft: "left.shift",
  ShiftRight: "right.shift",
  ControlLeft: "left.control",
  ControlRight: "right.control",
  AltLeft: "left.alt",
  AltRight: "right.alt",
  Minus: "minus",
  Equal: "equal",
  BracketLeft: "left.bracket",
  BracketRight: "right.bracket",
  Backslash: "backslash",
  Semicolon: "semicolon",
  Quote: "apostrophe",
  Comma: "comma",
  Period: "period",
  Slash: "slash",
  Backquote: "grave.accent",
  NumpadAdd: "keypad.add",
  NumpadSubtract: "keypad.subtract",
  NumpadMultiply: "keypad.multiply",
  NumpadDivide: "keypad.divide",
  NumpadDecimal: "keypad.decimal",
  NumpadEnter: "keypad.enter",
};

export const UNBOUND = "key.keyboard.unknown";

/** Returns a `key.keyboard.*` token, or null for an unmappable key. */
export function eventToToken(e: KeyboardEvent): string | null {
  const c = e.code;
  if (/^Key[A-Z]$/.test(c)) return `key.keyboard.${c.slice(3).toLowerCase()}`;
  if (/^Digit[0-9]$/.test(c)) return `key.keyboard.${c.slice(5)}`;
  if (/^F([1-9]|1[0-9]|2[0-5])$/.test(c)) return `key.keyboard.${c.toLowerCase()}`;
  if (/^Numpad[0-9]$/.test(c)) return `key.keyboard.keypad.${c.slice(6)}`;
  const mapped = CODE_TO_MC[c];
  return mapped ? `key.keyboard.${mapped}` : null;
}

/** MouseEvent.button -> Minecraft mouse token. */
export function mouseButtonToToken(button: number): string {
  if (button === 0) return "key.mouse.left";
  if (button === 1) return "key.mouse.middle";
  if (button === 2) return "key.mouse.right";
  return `key.mouse.${button + 1}`; // 3 -> mouse.4, 4 -> mouse.5
}

/** Pretty label for a token, mirroring the Rust side for fresh edits. */
export function tokenDisplay(token: string): string {
  if (token === UNBOUND) return "Unbound";
  const title = (s: string) =>
    s
      .split(/[._]/)
      .filter(Boolean)
      .map((w) => w[0].toUpperCase() + w.slice(1))
      .join(" ");
  if (token.startsWith("key.keyboard.")) return title(token.slice(13));
  if (token.startsWith("key.mouse.")) return `Mouse ${title(token.slice(10))}`;
  return token;
}
