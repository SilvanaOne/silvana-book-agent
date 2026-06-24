import { describe, it, expect, beforeEach } from 'vitest';
import { render, screen, act } from '@testing-library/react';
import { ThemeProvider, useTheme, readStoredTheme, THEME_PREPAINT } from './theme';

function Reader() {
  const { theme, toggle } = useTheme();
  return (
    <>
      <span data-testid="t">{theme}</span>
      <button onClick={toggle}>flip</button>
    </>
  );
}

describe('theme', () => {
  beforeEach(() => {
    window.localStorage.clear();
    document.documentElement.removeAttribute('data-theme');
  });

  it('defaults to dark when nothing is persisted', () => {
    render(
      <ThemeProvider>
        <Reader />
      </ThemeProvider>,
    );
    expect(screen.getByTestId('t').textContent).toBe('dark');
    expect(document.documentElement.getAttribute('data-theme')).toBe('dark');
  });

  it('reads a previously stored light choice on mount', () => {
    window.localStorage.setItem('arb.theme', 'light');
    render(
      <ThemeProvider>
        <Reader />
      </ThemeProvider>,
    );
    expect(screen.getByTestId('t').textContent).toBe('light');
    expect(document.documentElement.getAttribute('data-theme')).toBe('light');
  });

  it('toggle flips and persists', () => {
    render(
      <ThemeProvider>
        <Reader />
      </ThemeProvider>,
    );
    act(() => {
      screen.getByText('flip').click();
    });
    expect(screen.getByTestId('t').textContent).toBe('light');
    expect(window.localStorage.getItem('arb.theme')).toBe('light');
    act(() => {
      screen.getByText('flip').click();
    });
    expect(screen.getByTestId('t').textContent).toBe('dark');
  });

  it('readStoredTheme falls back to dark for garbage values', () => {
    window.localStorage.setItem('arb.theme', 'pink');
    expect(readStoredTheme()).toBe('dark');
  });

  it('THEME_PREPAINT references the storage key and sets data-theme', () => {
    expect(THEME_PREPAINT).toContain('arb.theme');
    expect(THEME_PREPAINT).toContain('data-theme');
  });
});
