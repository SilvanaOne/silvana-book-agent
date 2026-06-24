'use client';

import { useTheme } from '@/lib/theme';

/**
 * Sun/moon theme toggle. SVG-only; no icon font. Lives in the topbar.
 */
export function ThemeToggle() {
  const { theme, toggle } = useTheme();
  const isLight = theme === 'light';
  return (
    <button
      type="button"
      className="theme-toggle"
      onClick={toggle}
      aria-label={`switch to ${isLight ? 'dark' : 'light'} theme`}
      title={`switch to ${isLight ? 'dark' : 'light'} theme`}
    >
      <span className="theme-toggle__track" aria-hidden>
        <span className="theme-toggle__thumb" data-side={isLight ? 'right' : 'left'}>
          {isLight ? (
            <svg viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="4" />
              <path d="M12 2v3M12 19v3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M2 12h3M19 12h3M4.9 19.1L7 17M17 7l2.1-2.1" />
            </svg>
          ) : (
            <svg viewBox="0 0 24 24" width="13" height="13" fill="currentColor">
              <path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" />
            </svg>
          )}
        </span>
      </span>
    </button>
  );
}
