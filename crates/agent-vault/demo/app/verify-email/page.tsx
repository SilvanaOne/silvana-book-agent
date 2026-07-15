"use client";

import { useEffect, useState, type FormEvent } from "react";
import { useRouter } from "next/navigation";
import { ThemeToggle } from "../components/ThemeToggle";

export default function VerifyEmailPage() {
  const router = useRouter();
  const [email, setEmail] = useState("");
  const [code, setCode] = useState("");
  const [msg, setMsg] = useState("");
  const [ok, setOk] = useState(false);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    try {
      setEmail(sessionStorage.getItem("sv.verifyEmail") || "");
    } catch {
      /* ignore */
    }
  }, []);

  async function submit(e: FormEvent) {
    e.preventDefault();
    setBusy(true);
    setMsg("");
    setOk(false);
    if (!/^\d{6}$/.test(code)) {
      setMsg("Enter the 6-digit code");
      setBusy(false);
      return;
    }
    try {
      sessionStorage.removeItem("sv.verifyEmail");
    } catch {
      /* ignore */
    }
    router.push("/");
  }

  function resend() {
    setMsg("");
    setOk(true);
    setMsg("If an account exists, a code was sent. (Demo: any 6 digits pass.)");
  }

  return (
    <div className="auth-shell">
      <ThemeToggle fixed />
      <div className="shell">
        <form className="card" onSubmit={submit}>
          <h1 className="brand">
            <span>Verify email</span>
          </h1>
          <label>Email</label>
          <input
            type="email"
            required
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
          <label>6-digit code</label>
          <input
            inputMode="numeric"
            pattern="[0-9]{6}"
            required
            value={code}
            onChange={(e) => setCode(e.target.value)}
          />
          <button className="btn" type="submit" disabled={busy}>
            {busy ? "Verifying…" : "Verify"}
          </button>
          <button
            type="button"
            className="btn"
            style={{
              marginTop: 10,
              background: "transparent",
              border: "1px solid var(--border)",
              color: "var(--text)",
            }}
            onClick={resend}
          >
            Resend code
          </button>
          <div className={"msg" + (ok ? " ok" : msg ? " err" : "")}>{msg}</div>
        </form>
      </div>
    </div>
  );
}
