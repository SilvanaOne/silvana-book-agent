"use client";

import { useState, type MutableRefObject } from "react";
import { rsaEncrypt } from "./crypto-helpers";
import type { SecretSlot } from "@/lib/store";

type Props = {
  keyId: string;
  rsaKey: MutableRefObject<CryptoKey | null>;
  slots: SecretSlot[];
  onChange: () => Promise<void> | void;
};

export function LoadKeysTab({ keyId, rsaKey, slots, onChange }: Props) {
  return (
    <div>
      <div className="grid">
        <WalletForm keyId={keyId} rsaKey={rsaKey} onChange={onChange} />
        <CexForm keyId={keyId} rsaKey={rsaKey} onChange={onChange} />
      </div>

      <div className="card status">
        <h2>
          Vault status <span className="pill">{slots.length} slot(s)</span>
        </h2>
        <table>
          <thead>
            <tr>
              <th>kind</th>
              <th>venue / chain</th>
              <th>label</th>
              <th>party / pubkey</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {slots.length === 0 ? (
              <tr>
                <td colSpan={5} style={{ color: "var(--text-mute)" }}>
                  empty — load a key above
                </td>
              </tr>
            ) : (
              slots.map((s) => <SlotRow key={s.id} slot={s} onChange={onChange} />)
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function SlotRow({
  slot,
  onChange,
}: {
  slot: SecretSlot;
  onChange: () => Promise<void> | void;
}) {
  const label = slot.label || slot.venue || slot.id;
  const partyOrPub = (() => {
    if (slot.partyId)
      return (
        <span
          className="copy"
          onClick={() =>
            navigator.clipboard?.writeText(slot.partyId!).catch(() => {})
          }
        >
          {slot.partyId}
        </span>
      );
    if (slot.publicKeyHex)
      return (
        <span
          className="copy"
          title="pubkey hex"
          onClick={() =>
            navigator.clipboard
              ?.writeText(slot.publicKeyHex!)
              .catch(() => {})
          }
        >
          {slot.publicKeyHex.slice(0, 16)}…
        </span>
      );
    return "—";
  })();

  async function del() {
    if (!confirm("Delete slot " + label + "?")) return;
    const r = await fetch("/api/vault/secrets/" + encodeURIComponent(slot.id), {
      method: "DELETE",
    });
    if (r.ok) await onChange();
    else alert("delete failed");
  }

  if (slot.kind === "cex") {
    return (
      <tr>
        <td>cex</td>
        <td className="mono">{slot.venue}</td>
        <td>{slot.label || "—"}</td>
        <td className="mono">—</td>
        <td>
          <button type="button" className="del" title="delete" onClick={del}>
            🗑
          </button>
        </td>
      </tr>
    );
  }
  return (
    <tr>
      <td>wallet</td>
      <td className="mono">canton</td>
      <td>{slot.label || "—"}</td>
      <td className="mono">{partyOrPub}</td>
      <td>
        <button type="button" className="del" title="delete" onClick={del}>
          🗑
        </button>
      </td>
    </tr>
  );
}

function WalletForm({
  keyId,
  rsaKey,
  onChange,
}: {
  keyId: string;
  rsaKey: MutableRefObject<CryptoKey | null>;
  onChange: () => Promise<void> | void;
}) {
  const [label, setLabel] = useState("");
  const [partyId, setPartyId] = useState("");
  const [type, setType] = useState<"privKey" | "seed">("privKey");
  const [secret, setSecret] = useState("");
  const [msg, setMsg] = useState<{ text: string; ok: boolean }>({
    text: "",
    ok: false,
  });
  const [busy, setBusy] = useState(false);

  async function save() {
    setBusy(true);
    setMsg({ text: "", ok: false });
    try {
      if (!label.trim() || !secret.trim()) {
        setMsg({ text: "label and secret are required", ok: false });
        return;
      }
      setMsg({ text: "encrypting…", ok: true });
      const ct = await rsaEncrypt(rsaKey.current, secret);
      const body: Record<string, string> = {
        label,
        chain: "canton",
        keyId,
        [type === "seed" ? "seedCiphertext" : "privKeyCiphertext"]: ct,
      };
      if (partyId.trim()) body.partyId = partyId.trim();
      const r = await fetch("/api/vault/secrets/wallet", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const j = await r.json();
      if (!r.ok) throw new Error(j.error || "save failed");
      setSecret("");
      setMsg({ text: "stored ✓ (" + j.id + ")", ok: true });
      await onChange();
    } catch (e) {
      setMsg({ text: String((e as Error).message || e), ok: false });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="card">
      <h2>Canton wallet</h2>
      <label>Label</label>
      <input
        placeholder="e.g. canton-main / rebalancer"
        value={label}
        onChange={(e) => setLabel(e.target.value)}
      />
      <label>
        Party ID{" "}
        <span style={{ opacity: 0.65 }}>(optional — namespace::fingerprint)</span>
      </label>
      <input
        placeholder="e.g. myparty::1220abcd…"
        value={partyId}
        onChange={(e) => setPartyId(e.target.value)}
      />
      <label>Secret type</label>
      <select
        value={type}
        onChange={(e) => setType(e.target.value as "privKey" | "seed")}
      >
        <option value="privKey">Ed25519 private key (hex)</option>
        <option value="seed">Seed phrase (BIP39)</option>
      </select>
      <label>Secret value</label>
      <input
        type="password"
        placeholder="paste private key or seed"
        value={secret}
        onChange={(e) => setSecret(e.target.value)}
      />
      <button
        type="button"
        className="btn"
        onClick={save}
        disabled={busy}
      >
        {busy ? "Working…" : "Encrypt & store"}
      </button>
      <div className={"msg " + (msg.ok ? "ok" : msg.text ? "err" : "")}>
        {msg.text}
      </div>
      <div className="note">
        Public key hex is derived server-side for the status table. Signing
        stays in the engines (model B).
      </div>
    </div>
  );
}

function CexForm({
  keyId,
  rsaKey,
  onChange,
}: {
  keyId: string;
  rsaKey: MutableRefObject<CryptoKey | null>;
  onChange: () => Promise<void> | void;
}) {
  const [venue, setVenue] = useState("");
  const [label, setLabel] = useState("");
  const [apiKey, setApiKey] = useState("");
  const [secret, setSecret] = useState("");
  const [pass, setPass] = useState("");
  const [msg, setMsg] = useState<{ text: string; ok: boolean }>({
    text: "",
    ok: false,
  });
  const [busy, setBusy] = useState(false);

  async function save() {
    setBusy(true);
    setMsg({ text: "", ok: false });
    try {
      if (!venue || !apiKey.trim() || !secret.trim()) {
        setMsg({
          text: "venue, API key and secret are required",
          ok: false,
        });
        return;
      }
      setMsg({ text: "encrypting…", ok: true });
      const body: Record<string, string> = {
        venue,
        keyId,
        apiKeyCiphertext: await rsaEncrypt(rsaKey.current, apiKey),
        secretCiphertext: await rsaEncrypt(rsaKey.current, secret),
      };
      if (label.trim()) body.label = label.trim();
      if (pass.trim())
        body.passphraseCiphertext = await rsaEncrypt(rsaKey.current, pass);
      const r = await fetch("/api/vault/secrets/cex", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(body),
      });
      const j = await r.json();
      if (!r.ok) throw new Error(j.error || "save failed");
      setApiKey("");
      setSecret("");
      setPass("");
      setMsg({ text: "stored ✓ (" + j.id + ")", ok: true });
      await onChange();
    } catch (e) {
      setMsg({ text: String((e as Error).message || e), ok: false });
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="card">
      <h2>Exchange API key</h2>
      <label>Venue</label>
      <select value={venue} onChange={(e) => setVenue(e.target.value)}>
        <option value="">— select —</option>
        <option value="bybit">Bybit</option>
        <option value="kucoin">KuCoin</option>
      </select>
      <label>Label</label>
      <input
        placeholder="e.g. bybit-arb / kucoin-withdraw"
        value={label}
        onChange={(e) => setLabel(e.target.value)}
      />
      <label>API key</label>
      <input
        type="password"
        placeholder="API key"
        value={apiKey}
        onChange={(e) => setApiKey(e.target.value)}
      />
      <label>API secret</label>
      <input
        type="password"
        placeholder="API secret"
        value={secret}
        onChange={(e) => setSecret(e.target.value)}
      />
      <label>
        Passphrase <span style={{ opacity: 0.65 }}>(KuCoin required)</span>
      </label>
      <input
        type="password"
        placeholder="optional for Bybit"
        value={pass}
        onChange={(e) => setPass(e.target.value)}
      />
      <button
        type="button"
        className="btn"
        onClick={save}
        disabled={busy}
      >
        {busy ? "Working…" : "Encrypt & store"}
      </button>
      <div className={"msg " + (msg.ok ? "ok" : msg.text ? "err" : "")}>
        {msg.text}
      </div>
      <div className="note">
        Use trade-scoped keys for the executor and withdraw-scoped keys for the
        rebalancer where possible.
      </div>
    </div>
  );
}
