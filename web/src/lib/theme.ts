// Which of the two modes the phone page is in, and where that is remembered.
//
// The mode is a class on `<html>`: `dark` or `light`. `data-theme` keeps naming
// the Skeleton theme (it must stay `voice` for the kit's tokens), while the
// class is what our own token blocks and Tailwind's dark variant read. Setting
// `color-scheme` alongside it is what tells the browser — and Skeleton's
// `light-dark()` pairs — which way round the page is, so form controls and the
// kit's own components follow without a reload.
//
// The choice belongs to the phone, not the computer: `localStorage`, read once
// before the page is mounted so the first paint is already in the right mode.

export type ThemeMode = 'dark' | 'light';

const KEY = 'vb_theme';

/** The browser's own chrome (address bar, status bar) per mode. */
const THEME_COLOR: Record<ThemeMode, string> = {
  dark: '#111713',
  light: '#f4f7ef',
};

/** The saved mode; dark when nothing was saved or storage is denied. */
export function readTheme(): ThemeMode {
  try {
    return localStorage.getItem(KEY) === 'light' ? 'light' : 'dark';
  } catch {
    return 'dark';
  }
}

/** Put one mode on the page, and remember it for the next visit. */
export function applyTheme(mode: ThemeMode): void {
  const root = document.documentElement;
  root.classList.toggle('dark', mode === 'dark');
  root.classList.toggle('light', mode === 'light');
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta instanceof HTMLMetaElement) meta.content = THEME_COLOR[mode];
  try {
    localStorage.setItem(KEY, mode);
  } catch {
    // Private mode: the mode lasts this visit, and dark is the default.
  }
}
