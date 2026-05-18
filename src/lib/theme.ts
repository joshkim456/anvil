import type { ThemePref } from "./api";

// `data-theme` on <html> drives the palette (see :root[data-theme="dark"] in
// styles.css). "system" tracks the OS and keeps following it live.

const mq = window.matchMedia("(prefers-color-scheme: dark)");
let systemListener: ((e: MediaQueryListEvent) => void) | null = null;

function resolve(pref: ThemePref): "light" | "dark" {
  if (pref === "system") return mq.matches ? "dark" : "light";
  return pref;
}

// Backend Settings is the source of truth, but it's read async on boot. We
// also mirror the preference into localStorage so the inline script in
// index.html can set the right palette synchronously, before first paint
// (no light-then-dark flash).
export const THEME_CACHE_KEY = "anvil-theme";

export function applyTheme(pref: ThemePref) {
  try {
    localStorage.setItem(THEME_CACHE_KEY, pref);
  } catch {
    // private mode / storage disabled: just skip the pre-paint cache
  }
  document.documentElement.dataset.theme = resolve(pref);

  // Only keep an OS listener while in "system" mode; clear it otherwise so an
  // explicit choice isn't overridden when the OS flips.
  if (systemListener) {
    mq.removeEventListener("change", systemListener);
    systemListener = null;
  }
  if (pref === "system") {
    systemListener = () => {
      document.documentElement.dataset.theme = mq.matches ? "dark" : "light";
    };
    mq.addEventListener("change", systemListener);
  }
}
