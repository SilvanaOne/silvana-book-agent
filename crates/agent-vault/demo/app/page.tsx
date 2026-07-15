"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { ThemeToggle } from "./components/ThemeToggle";
import { LoadKeysTab } from "./components/LoadKeysTab";
import { BalancesTab } from "./components/BalancesTab";
import { pemToDer } from "./components/crypto-helpers";
import type { SecretSlot } from "@/lib/store";

type Me = {
  kind: "tenant" | "admin" | "anonymous";
  userId?: string;
  username?: string;
  email?: string;
};

type Tab = "keys" | "balances";

export default function DashboardPage() {
  const router = useRouter();
  const [me, setMe] = useState<Me | null>(null);
  const [keyId, setKeyId] = useState<string>("");
  const rsaKeyRef = useRef<CryptoKey | null>(null);
  const [slots, setSlots] = useState<SecretSlot[]>([]);
  const [tab, setTab] = useState<Tab>("keys");
  const [copied, setCopied] = useState(false);

  const loadStatus = useCallback(async () => {
    const r = await fetch("/api/vault/status", { cache: "no-store" });
    if (!r.ok) return;
    const j = (await r.json()) as { slots: SecretSlot[] };
    setSlots(j.slots ?? []);
  }, []);

  useEffect(() => {
    let cancelled = false;
    (async () => {
      const meRes = await fetch("/api/vault/auth/me", { cache: "no-store" });
      const meJson = (await meRes.json()) as Me;
      if (cancelled) return;
      if (meJson.kind === "anonymous" || !meJson.userId) {
        router.replace("/login");
        return;
      }
      setMe(meJson);

      const pk = await fetch("/api/vault/pubkey").then((r) => r.json());
      setKeyId(pk.keyId);
      try {
        const der = pemToDer(pk.pem);
        rsaKeyRef.current = await crypto.subtle.importKey(
          "spki",
          der,
          { name: "RSA-OAEP", hash: "SHA-256" },
          false,
          ["encrypt"],
        );
      } catch {
        rsaKeyRef.current = null;
      }
      await loadStatus();
    })();
    return () => {
      cancelled = true;
    };
  }, [loadStatus, router]);

  async function logout() {
    try {
      await fetch("/api/vault/auth/logout", { method: "POST" });
    } catch {
      /* ignore */
    }
    router.push("/login");
  }

  function copyUid() {
    if (!me?.userId) return;
    navigator.clipboard?.writeText(me.userId).catch(() => {});
    setCopied(true);
    setTimeout(() => setCopied(false), 1400);
  }

  if (!me) {
    return (
      <div className="wrap">
        <div className="note">Loading vault…</div>
      </div>
    );
  }

  return (
    <div className="wrap">
      <header className="topbar">
        <div>
          <h1 className="brand">
            <span>Silvana</span> Vault
          </h1>
          <div className="sub">
            Canton wallet keys + Bybit / KuCoin API keys. Encrypted
            in-browser (RSA-OAEP-SHA256); only ciphertext is stored.
          </div>
          <div className="key">
            {keyId ? `vault key ${keyId}` : "loading key…"}
          </div>
        </div>
        <div style={{ display: "flex", alignItems: "center", gap: "0.75rem" }}>
          <ThemeToggle />
          <button type="button" className="btn-ghost" onClick={logout}>
            Log out
          </button>
        </div>
      </header>

      <div className="card" style={{ marginBottom: 16 }}>
        <h2>
          Agent User ID{" "}
          <span className="pill">
            {me.username || me.email || "tenant"}
          </span>
        </h2>
        <div className="note">
          Put this UUID in your agent config as{" "}
          <span style={{ fontFamily: "var(--font-mono)" }}>
            SIGNER_TENANT_USER_ID
          </span>
          .
        </div>
        <div
          style={{
            marginTop: 10,
            fontFamily: "var(--font-mono)",
            fontSize: ".75rem",
            padding: "10px 12px",
            background: "var(--bg)",
            border: "1px solid var(--border-soft)",
            borderRadius: 10,
            wordBreak: "break-all",
          }}
        >
          <span
            className="copy"
            onClick={copyUid}
            title="click to copy"
          >
            {me.userId}
          </span>
          {copied ? (
            <span
              style={{
                marginLeft: 8,
                color: "var(--good)",
                fontSize: ".7rem",
              }}
            >
              copied ✓
            </span>
          ) : null}
        </div>
      </div>

      <div className="tabs">
        <button
          className={"tab" + (tab === "keys" ? " active" : "")}
          onClick={() => setTab("keys")}
          type="button"
        >
          Load keys
        </button>
        <button
          className={"tab" + (tab === "balances" ? " active" : "")}
          onClick={() => setTab("balances")}
          type="button"
        >
          Balances
        </button>
      </div>

      {tab === "keys" ? (
        <LoadKeysTab
          keyId={keyId}
          rsaKey={rsaKeyRef}
          slots={slots}
          onChange={loadStatus}
        />
      ) : (
        <BalancesTab />
      )}

      <div
        style={{
          marginTop: 32,
          display: "flex",
          justifyContent: "center",
        }}
      >
        <span className="hosted">
          HOSTED ON
          <img src="/silvana-logo.svg" alt="silvana" />
        </span>
      </div>
    </div>
  );
}
