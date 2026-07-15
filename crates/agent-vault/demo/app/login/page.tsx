"use client";

import { useState, type FormEvent } from "react";
import { useRouter } from "next/navigation";
import { ThemeToggle } from "../components/ThemeToggle";

export default function LoginPage() {
  const router = useRouter();
  const [emailOrUsername, setEmailOrUsername] = useState("");
  const [password, setPassword] = useState("");
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setMsg("");
    try {
      const res = await fetch("/api/auth/login", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ emailOrUsername, password }),
      });
      const j = await res.json().catch(() => ({}));
      if (res.ok) {
        router.push("/");
        return;
      }
      setMsg(j.error || "Invalid login or password");
    } catch {
      setMsg("Invalid login or password");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="auth-shell">
      <ThemeToggle fixed />
      <div className="shell">
        <form className="card" onSubmit={submit}>
          <h1 className="brand">
            <span>Sign in</span> — Silvana Vault
          </h1>
          <label htmlFor="u">Email or username</label>
          <input
            id="u"
            autoComplete="username"
            autoFocus
            required
            value={emailOrUsername}
            onChange={(e) => setEmailOrUsername(e.target.value)}
          />
          <label htmlFor="p">Password</label>
          <input
            id="p"
            type="password"
            autoComplete="current-password"
            required
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <button className="btn" type="submit" disabled={busy}>
            {busy ? "Signing in…" : "Sign in"}
          </button>
          <div className={"msg" + (msg ? " err" : "")}>{msg}</div>
          <div className="link">
            No account? <a href="/register">Create one</a>
          </div>
          <div
            className="note"
            style={{ textAlign: "center", marginTop: "18px" }}
          >
            Demo mode — any credentials work. State is per-server-process, in memory.
          </div>
        </form>
      </div>
    </div>
  );
}
