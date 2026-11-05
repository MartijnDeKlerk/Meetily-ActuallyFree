'use client';

import { useEffect, useState } from 'react';

/**
 * App themes. Colors live in globals.css as channel tokens under
 * `:root[data-theme='…']`; this module only decides which theme is active.
 *
 * The choice is stored in localStorage, which the main window and the minibar
 * share, so a change in Settings repaints both.
 */
export type AppTheme = 'midnight' | 'vanilla' | 'latte' | 'charcoal';

/** What the user picked: a fixed theme, or follow the operating system's light/dark setting. */
export type ThemePreference = AppTheme | 'system';

export interface ThemeInfo {
  id: AppTheme;
  label: string;
  description: string;
  dark: boolean;
  /** Canvas, panel, raised surface, text, accent: used for theme previews. */
  swatch: [string, string, string, string, string];
}

export const THEMES: ThemeInfo[] = [
  {
    id: 'midnight',
    label: 'Midnight',
    description: 'Deep navy. The default.',
    dark: true,
    swatch: ['#0a0c10', '#0f1218', '#181d27', '#e6edf5', '#3b7cf0'],
  },
  {
    id: 'vanilla',
    label: 'Vanilla',
    description: 'Warm cream with ink-blue accents.',
    dark: false,
    swatch: ['#efe8dc', '#faf6ef', '#fffcf7', '#2a2319', '#2f5ecf'],
  },
  {
    id: 'latte',
    label: 'Latte',
    description: 'Cool Catppuccin Latte with mauve accents.',
    dark: false,
    swatch: ['#dce0e8', '#eff1f5', '#f8f9fb', '#4c4f69', '#8839ef'],
  },
  {
    id: 'charcoal',
    label: 'Charcoal',
    description: 'Warm charcoal with clay accents.',
    dark: true,
    swatch: ['#141211', '#1b1917', '#262321', '#f0ede9', '#de8462'],
  },
];

export const DEFAULT_THEME: AppTheme = 'midnight';
const THEME_STORAGE_KEY = 'meetily_theme';
/** Dark theme used while following the system; remembers the last dark theme picked. */
const DARK_THEME_STORAGE_KEY = 'meetily_theme_dark';
/** Light theme used while following the system; remembers the last light theme picked. */
const LIGHT_THEME_STORAGE_KEY = 'meetily_theme_light';
const DEFAULT_LIGHT_THEME: AppTheme = 'vanilla';
const THEME_EVENT = 'af-theme-change';

function normalizeTheme(value: string | null): AppTheme {
  if (value === 'midnight' || value === 'vanilla' || value === 'latte' || value === 'charcoal') return value;
  // Stored by the old light/dark toggle.
  if (value === 'light') return 'vanilla';
  return DEFAULT_THEME;
}

function normalizePreference(value: string | null): ThemePreference {
  return value === 'system' ? 'system' : normalizeTheme(value);
}

/**
 * The OS color scheme as reported by the native side (XDG Settings portal on
 * Linux). WebKitGTK's `prefers-color-scheme` does not follow KDE's setting, so
 * this wins when known; `null` means fall back to the media query. The last
 * value is cached and used provisionally from startup, matching the boot
 * script's first paint, until the native read replaces it.
 */
const SYSTEM_DARK_STORAGE_KEY = 'meetily_system_dark';
let nativeSystemDark: boolean | null | undefined;

function systemPrefersDark(): boolean {
  if (typeof window === 'undefined') return false;
  if (nativeSystemDark === undefined) {
    try {
      const cached = localStorage.getItem(SYSTEM_DARK_STORAGE_KEY);
      nativeSystemDark = cached === null ? null : cached === '1';
    } catch {
      nativeSystemDark = null;
    }
  }
  if (nativeSystemDark !== null) return nativeSystemDark;
  return window.matchMedia('(prefers-color-scheme: dark)').matches;
}

/** `null` = the OS has no preference: drop the override and the cache. */
function setNativeSystemDark(dark: boolean | null) {
  nativeSystemDark = dark;
  try {
    if (dark === null) localStorage.removeItem(SYSTEM_DARK_STORAGE_KEY);
    else localStorage.setItem(SYSTEM_DARK_STORAGE_KEY, dark ? '1' : '0');
  } catch {
    // Cache only; the live value above still applies.
  }
}

function schemeToDark(scheme: string | null): boolean | null {
  return scheme === 'dark' ? true : scheme === 'light' ? false : null;
}

/** Follow the native OS scheme. Returns an unsubscribe. */
function watchNativeSystemScheme(onChange: () => void): () => void {
  if (!('__TAURI_INTERNALS__' in window)) return () => {};
  let cancelled = false;
  let unlisten: (() => void) | undefined;
  void (async () => {
    // An event that arrives before the initial read is newer than that read.
    let eventSeen = false;
    try {
      const [{ invoke }, { listen }] = await Promise.all([
        import('@tauri-apps/api/core'),
        import('@tauri-apps/api/event'),
      ]);
      // Subscribe before reading, so a change during the read is not lost.
      const stop = await listen<string>('system-color-scheme-changed', (event) => {
        eventSeen = true;
        setNativeSystemDark(schemeToDark(event.payload));
        onChange();
      });
      if (cancelled) {
        stop();
        return;
      }
      unlisten = stop;
      const initial = await invoke<string | null>('get_system_color_scheme');
      if (cancelled || eventSeen) return;
      // `null` (portal unavailable) clears a scheme cached by an earlier run, so
      // a stale override cannot outlive the desktop that produced it.
      setNativeSystemDark(schemeToDark(initial));
      onChange();
    } catch (error) {
      console.warn('Native color scheme unavailable; using prefers-color-scheme:', error);
      if (!cancelled && !eventSeen) {
        setNativeSystemDark(null);
        onChange();
      }
    }
  })();
  return () => {
    cancelled = true;
    unlisten?.();
  };
}

function savedLightTheme(): AppTheme {
  try {
    const saved = normalizeTheme(localStorage.getItem(LIGHT_THEME_STORAGE_KEY));
    return themeInfo(saved).dark ? DEFAULT_LIGHT_THEME : saved;
  } catch {
    return DEFAULT_LIGHT_THEME;
  }
}

function savedDarkTheme(): AppTheme {
  try {
    const saved = normalizeTheme(localStorage.getItem(DARK_THEME_STORAGE_KEY));
    return themeInfo(saved).dark ? saved : DEFAULT_THEME;
  } catch {
    return DEFAULT_THEME;
  }
}

/**
 * Keep the fixed theme the user is on before "system" replaces it, so
 * switching to system and back (or upgrading with Charcoal selected) does not
 * lose it. Must run before the system preference is resolved or persisted.
 */
function rememberFixedTheme() {
  try {
    const current = localStorage.getItem(THEME_STORAGE_KEY);
    if (current === 'midnight' || current === 'charcoal') {
      localStorage.setItem(DARK_THEME_STORAGE_KEY, current);
    } else if (current === 'vanilla' || current === 'latte') {
      localStorage.setItem(LIGHT_THEME_STORAGE_KEY, current);
    }
  } catch {
    // Falls back to the default dark theme.
  }
}

/** The themes system mode uses for each OS scheme. */
export function systemThemePair(): { light: AppTheme; dark: AppTheme } {
  return { light: savedLightTheme(), dark: savedDarkTheme() };
}

/** The concrete theme a preference paints right now. */
export function resolveThemePreference(preference: ThemePreference): AppTheme {
  if (preference !== 'system') return preference;
  return systemPrefersDark() ? savedDarkTheme() : savedLightTheme();
}

export function themeInfo(theme: AppTheme): ThemeInfo {
  return THEMES.find((entry) => entry.id === theme) ?? THEMES[0];
}

export function getSavedThemePreference(): ThemePreference {
  if (typeof window === 'undefined') return DEFAULT_THEME;
  try {
    return normalizePreference(localStorage.getItem(THEME_STORAGE_KEY));
  } catch {
    return DEFAULT_THEME;
  }
}

/** The theme currently in effect, after resolving a "system" preference. */
export function getSavedAppTheme(): AppTheme {
  return resolveThemePreference(getSavedThemePreference());
}

/**
 * Paint the document with `theme`. `persist` also saves it as the preference
 * (`preference`, when the user chose "system") and tells other windows.
 * `followSystem` leaves the native window theme unforced so the OS setting,
 * and therefore prefers-color-scheme, keeps flowing through.
 */
export function applyAppTheme(theme: AppTheme, persist = false, preference: ThemePreference = theme) {
  if (typeof window === 'undefined') return;
  const info = themeInfo(theme);
  const followSystem = preference === 'system';
  const root = document.documentElement;
  root.setAttribute('data-theme', info.id);
  root.classList.toggle('dark', info.dark);

  if (persist) {
    try {
      localStorage.setItem(THEME_STORAGE_KEY, preference);
      if (!followSystem) {
        localStorage.setItem(info.dark ? DARK_THEME_STORAGE_KEY : LIGHT_THEME_STORAGE_KEY, info.id);
      }
    } catch {
      // Storage can be unavailable in locked-down webviews; the paint still applies.
    }
  }
  window.dispatchEvent(new CustomEvent<ThemePreference>(THEME_EVENT, { detail: preference }));

  if ('__TAURI_INTERNALS__' in window) {
    void import('@tauri-apps/api/window')
      .then(({ getCurrentWindow }) => getCurrentWindow().setTheme(followSystem ? null : info.dark ? 'dark' : 'light'))
      .catch((error) => console.warn('Failed to sync the native window theme:', error));
  }
}

/**
 * Runs in <head> before the first paint so the window never flashes the
 * default theme. Kept dependency-free because it is inlined as a string.
 */
export const THEME_BOOT_SCRIPT = `
(function () {
  try {
    var saved = localStorage.getItem('${THEME_STORAGE_KEY}');
    var darkSaved = localStorage.getItem('${DARK_THEME_STORAGE_KEY}');
    var lightSaved = localStorage.getItem('${LIGHT_THEME_STORAGE_KEY}');
    var theme = saved === 'vanilla' || saved === 'latte' || saved === 'charcoal' || saved === 'midnight'
      ? saved
      : (saved === 'light' ? 'vanilla' : 'midnight');
    if (saved === 'system') {
      var cached = localStorage.getItem('${SYSTEM_DARK_STORAGE_KEY}');
      var dark = cached === null
        ? window.matchMedia('(prefers-color-scheme: dark)').matches
        : cached === '1';
      theme = dark
        ? (darkSaved === 'charcoal' ? 'charcoal' : 'midnight')
        : (lightSaved === 'latte' ? 'latte' : 'vanilla');
    }
    var root = document.documentElement;
    root.setAttribute('data-theme', theme);
    if (theme === 'vanilla' || theme === 'latte') root.classList.remove('dark');
    else root.classList.add('dark');
  } catch (_) {}
})();
`;

/**
 * The user's theme preference, kept in sync with Settings and with the other
 * app window. While it is "system", also repaints when the OS flips between
 * light and dark (for example an evening dark-mode schedule).
 */
export function useThemePreference(): [ThemePreference, (preference: ThemePreference) => void] {
  const [preference, setPreference] = useState<ThemePreference>(DEFAULT_THEME);

  useEffect(() => {
    setPreference(getSavedThemePreference());
    const onLocal = (event: Event) => setPreference((event as CustomEvent<ThemePreference>).detail);
    const onStorage = (event: StorageEvent) => {
      if (event.key !== THEME_STORAGE_KEY) return;
      const next = normalizePreference(event.newValue);
      applyAppTheme(resolveThemePreference(next), false, next);
      setPreference(next);
    };
    window.addEventListener(THEME_EVENT, onLocal);
    window.addEventListener('storage', onStorage);
    return () => {
      window.removeEventListener(THEME_EVENT, onLocal);
      window.removeEventListener('storage', onStorage);
    };
  }, []);

  useEffect(() => {
    if (preference !== 'system') return;
    const query = window.matchMedia('(prefers-color-scheme: dark)');
    const repaint = () => applyAppTheme(resolveThemePreference('system'), false, 'system');
    repaint();
    query.addEventListener('change', repaint);
    const stopNative = watchNativeSystemScheme(repaint);
    return () => {
      query.removeEventListener('change', repaint);
      stopNative();
    };
  }, [preference]);

  return [
    preference,
    (next: ThemePreference) => {
      if (next === 'system') rememberFixedTheme();
      applyAppTheme(resolveThemePreference(next), true, next);
    },
  ];
}

/** The concrete theme in effect (a "system" preference is resolved), and a setter for a fixed theme. */
export function useAppTheme(): [AppTheme, (theme: AppTheme) => void] {
  const [preference, setPreference] = useThemePreference();
  // Re-resolve on every render: the system query repaint above changes the
  // document theme but not the preference, so read the live attribute instead.
  const [, force] = useState(0);
  useEffect(() => {
    const bump = () => force((n) => n + 1);
    window.addEventListener(THEME_EVENT, bump);
    return () => window.removeEventListener(THEME_EVENT, bump);
  }, []);
  return [resolveThemePreference(preference), setPreference];
}
