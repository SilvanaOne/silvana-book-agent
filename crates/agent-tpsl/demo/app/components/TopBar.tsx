"use client";

import { useState } from "react";

type Tab = "dashboard" | "events" | "docs";

type Props = Readonly<{ active?: Tab; onChange?: (t: Tab) => void }>;

export function TopBar({ active = "dashboard", onChange }: Props) {
  const [tab, setTab] = useState<Tab>(active);
  const click = (t: Tab) => {
    setTab(t);
    onChange?.(t);
  };
  return (
    <>
      <div className="topbar">
        <div className="topbar-inner">
          <div className="brand">
            <span className="brand-mark">S</span>
            Silvana
            <span className="brand-sep">·</span>
            <span className="brand-agent mono">agent-tpsl</span>
            <span className="badge canton">Canton</span>
            <span className="badge agent">Agent</span>
            <span className="badge demo">Demo</span>
          </div>
          <nav className="nav">
            <button className={tab === "dashboard" ? "active" : ""} onClick={() => click("dashboard")}>Dashboard</button>
            <button className={tab === "events" ? "active" : ""} onClick={() => click("events")}>Events</button>
            <button className={tab === "docs" ? "active" : ""} onClick={() => click("docs")}>Docs</button>
          </nav>
          <div className="topright">
            <a href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">GitHub</a>
            <a href="https://docs.silvana.one" target="_blank" rel="noopener">Guide</a>
          </div>
        </div>
      </div>
    </>
  );
}
