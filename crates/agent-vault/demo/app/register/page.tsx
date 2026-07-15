"use client";

import { useState, type FormEvent } from "react";
import { useRouter } from "next/navigation";
import { ThemeToggle } from "../components/ThemeToggle";

export default function RegisterPage() {
  const router = useRouter();
  const [username, setUsername] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [password2, setPassword2] = useState("");
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setMsg("");
    if (password !== password2) {
      setMsg("Passwords do not match");
      setBusy(false);
      return;
    }
    try {
      const res = await fetch("/api/auth/register", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ username, email, password }),
      });
      const j = await res.json().catch(() => ({}));
      if (!res.ok) {
        setMsg(j.error || "Registration failed");
        return;
      }
      if (j.sessionId) {
        router.push("/");
        return;
      }
      try {
        sessionStorage.setItem("sv.verifyEmail", email);
      } catch {
        /* ignore */
      }
      router.push("/verify-email");
    } catch {
      setMsg("Registration failed");
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
            <span>Register</span> — Silvana Vault
          </h1>
          <label>Username</label>
          <input
            autoComplete="username"
            required
            minLength={3}
            value={username}
            onChange={(e) => setUsername(e.target.value)}
          />
          <label>Email</label>
          <input
            type="email"
            autoComplete="email"
            required
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
          <label>Password</label>
          <input
            type="password"
            autoComplete="new-password"
            required
            minLength={8}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          <label>Retype Password</label>
          <input
            type="password"
            autoComplete="new-password"
            required
            minLength={8}
            value={password2}
            onChange={(e) => setPassword2(e.target.value)}
          />
          <button className="btn" type="submit" disabled={busy}>
            {busy ? "Creating…" : "Create account"}
          </button>
          <div className={"msg" + (msg ? " err" : "")}>{msg}</div>
          <div className="link">
            Already have an account? <a href="/login">Sign in</a>
          </div>
          <div
            className="note"
            style={{ textAlign: "center", marginTop: "18px" }}
          >
            Demo mode — instant registration. Password is not persisted.
          </div>
        </form>
      </div>
    </div>
  );
}
