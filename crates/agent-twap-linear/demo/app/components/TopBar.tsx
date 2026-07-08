"use client";

import { useEffect, useState } from "react";

type Tab = "dashboard" | "events" | "docs";

type Props = Readonly<{ active?: Tab; onChange?: (t: Tab) => void; live: boolean }>;

export function TopBar({ active = "dashboard", onChange, live }: Props) {
  const [tab, setTab] = useState<Tab>(active);
  const [now, setNow] = useState("");
  useEffect(() => {
    const upd = () => {
      const d = new Date();
      const pad = (n: number) => n.toString().padStart(2, "0");
      setNow(`${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`);
    };
    upd();
    const id = setInterval(upd, 1000);
    return () => clearInterval(id);
  }, []);
  const click = (t: Tab) => { setTab(t); onChange?.(t); };
  return (
    <>
      <div className="topbar">
        <div className="brand">
          <span className="brand-mark">T</span>
          <span className="brand-title">TWAP</span>
          <span className="brand-agent">Agent</span>
          <span className="chip">CANTON</span>
        </div>
        <nav className="nav">
          <button className={tab === "dashboard" ? "active" : ""} onClick={() => click("dashboard")}>Dashboard</button>
          <button className={tab === "events" ? "active" : ""} onClick={() => click("events")}>Events</button>
          <button className={tab === "docs" ? "active" : ""} onClick={() => click("docs")}>Docs</button>
          <a href="https://github.com/SilvanaOne/silvana-book-agent" target="_blank" rel="noopener">GitHub</a>
          <a href="https://docs.silvana.one" target="_blank" rel="noopener">Guide</a>
        </nav>
      </div>
      <div className="statusrow">
        <span className="status-live" style={{ color: live ? "var(--pos)" : "var(--text-faint)" }}>
          <span className="dot" style={{ background: live ? "var(--pos)" : "var(--text-faint)", boxShadow: live ? "0 0 8px var(--pos)" : "none" }} />
          {live ? "live" : "offline"}
        </span>
        <span className="status-time mono">{now}</span>
        <span className="status-role">operator</span>
        <span className="toggle" />
        <span className="hosted">
          HOSTED ON
          <a href="https://silvana.one" target="_blank" rel="noopener" className="hosted-logo">
            {/* eslint-disable-next-line @next/next/no-img-element */}
            <img src="/silvana-mark.svg" alt="" width={18} height={18} />
            <span>silvana</span>
          </a>
        </span>
      </div>
    </>
  );
}
