"use client";

import { useEffect, useState } from "react";

type Theme = "light" | "dark";
const THEME_KEY = "sv.theme";

function readInitial(): Theme {
  if (typeof document === "undefined") return "dark";
  const attr = document.documentElement.getAttribute("data-theme");
  return attr === "light" ? "light" : "dark";
}

export function ThemeToggle({ fixed = false }: { fixed?: boolean }) {
  const [theme, setTheme] = useState<Theme>("dark");

  useEffect(() => {
    setTheme(readInitial());
  }, []);

  function apply(next: Theme) {
    setTheme(next);
    document.documentElement.setAttribute("data-theme", next);
    try {
      localStorage.setItem(THEME_KEY, next);
    } catch {
      /* ignore */
    }
  }

  const side = theme === "light" ? "right" : "left";
  const glyph = theme === "light" ? "☀" : "🌙";

  return (
    <button
      type="button"
      className={"theme-toggle" + (fixed ? " theme-fixed" : "")}
      onClick={() => apply(theme === "light" ? "dark" : "light")}
      aria-label="Toggle theme"
      title="Toggle light/dark theme"
    >
      <span className="theme-toggle__track">
        <span className="theme-toggle__thumb" data-side={side}>
          {glyph}
        </span>
      </span>
    </button>
  );
}
