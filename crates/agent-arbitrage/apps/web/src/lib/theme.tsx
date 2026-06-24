'use client';

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';

export type Theme = 'dark' | 'light';

interface ThemeCtxValue {
  readonly theme: Theme;
  readonly setTheme: (t: Theme) => void;
  readonly toggle: () => void;
}

const ThemeCtx = createContext<ThemeCtxValue | null>(null);
const STORAGE_KEY = 'arb.theme';

/** Read the persisted choice (falls back to dark — the original product surface). */
export function readStoredTheme(): Theme {
  if (typeof window === 'undefined') return 'dark';
  const v = window.localStorage.getItem(STORAGE_KEY);
  return v === 'light' ? 'light' : 'dark';
}

/**
 * Inline script injected before hydration so the document is painted in the
 * right theme on first frame (no white flash on a saved-light reload, no dark
 * flash on a saved-dark reload).
 */
export const THEME_PREPAINT = `try{var t=localStorage.getItem(${JSON.stringify(STORAGE_KEY)});if(t==='light'||t==='dark')document.documentElement.setAttribute('data-theme',t);}catch(e){}`;

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>('dark');

  // pick up stored choice after mount (the prepaint script handles first paint)
  useEffect(() => {
    const t = readStoredTheme();
    setThemeState(t);
  }, []);

  // mirror choice to <html data-theme> + persist
  useEffect(() => {
    if (typeof document !== 'undefined') {
      document.documentElement.setAttribute('data-theme', theme);
    }
    if (typeof window !== 'undefined') {
      try {
        window.localStorage.setItem(STORAGE_KEY, theme);
      } catch {
        /* private mode etc — ignore */
      }
    }
  }, [theme]);

  const setTheme = useCallback((t: Theme) => setThemeState(t), []);
  const toggle = useCallback(() => setThemeState((cur) => (cur === 'dark' ? 'light' : 'dark')), []);

  const value = useMemo(() => ({ theme, setTheme, toggle }), [theme, setTheme, toggle]);
  return <ThemeCtx.Provider value={value}>{children}</ThemeCtx.Provider>;
}

export function useTheme(): ThemeCtxValue {
  const v = useContext(ThemeCtx);
  if (!v) throw new Error('useTheme must be inside <ThemeProvider>');
  return v;
}
