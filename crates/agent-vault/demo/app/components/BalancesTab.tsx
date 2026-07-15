"use client";

import { useCallback, useEffect, useState } from "react";
import type { BalancesSnapshot, BalanceRow } from "@/lib/store";

const fmt = (n: number) =>
  Number(n).toLocaleString("en-US", { maximumFractionDigits: 6 });
const usd = (n: number | null | undefined) =>
  n === null || n === undefined
    ? "—"
    : "$" + Number(n).toLocaleString("en-US", { maximumFractionDigits: 2 });
const short = (a: string | undefined) =>
  a ? a.slice(0, 8) + "…" + a.slice(-4) : "";

export function BalancesTab() {
  const [snap, setSnap] = useState<BalancesSnapshot | null>(null);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string>("");

  const load = useCallback(async () => {
    setBusy(true);
    setErr("");
    try {
      const r = await fetch("/api/vault/balances", { cache: "no-store" });
      const j = (await r.json()) as BalancesSnapshot & { error?: string };
      if (j.error) throw new Error(j.error);
      setSnap(j as BalancesSnapshot);
    } catch (e) {
      setErr(String((e as Error).message || e));
    } finally {
      setBusy(false);
    }
  }, []);

  useEffect(() => {
    load();
    const t = setInterval(load, 60_000);
    return () => clearInterval(t);
  }, [load]);

  const rows = snap?.rows ?? [];
  const sources = snap?.sources ?? [];
  const byId: Record<
    string,
    { chain: string; address?: string; coins: BalanceRow[] }
  > = {};
  for (const r of rows) {
    const g = (byId[r.sourceId] = byId[r.sourceId] || {
      chain: r.venueOrChain,
      address: undefined,
      coins: [],
    });
    if (r.address) g.address = r.address;
    if (r.asset && r.asset !== "—" && (r.free || r.locked || r.usd))
      g.coins.push(r);
  }

  return (
    <div className="card">
      <div className="bar">
        <button
          type="button"
          className="btn"
          style={{ width: "auto", margin: 0, padding: ".55rem 1rem" }}
          onClick={load}
          disabled={busy}
        >
          {busy ? "Scanning…" : "Refresh now"}
        </button>
        <div style={{ textAlign: "right" }}>
          <div style={{ fontSize: ".75rem", color: "var(--text-mute)" }}>
            {snap?.updatedAt
              ? "updated " + new Date(snap.updatedAt).toLocaleTimeString()
              : ""}
          </div>
          <div className="total">{usd(snap?.totalUsd)}</div>
        </div>
      </div>

      {err ? <div className="err-row">{err}</div> : null}

      <div className="bcards">
        {sources.length === 0 ? (
          <div style={{ color: "var(--text-mute)" }}>
            no keys loaded yet
          </div>
        ) : (
          sources.map((src) => {
            const g = byId[src.id] || { chain: "", address: undefined, coins: [] };
            return (
              <div key={src.id} className="bcard">
                <div className="bcard-h">
                  <div>
                    <span className="lab">{src.label}</span>{" "}
                    <span className="pill">
                      {src.kind === "canton" ? "CANTON" : "CEX"}
                    </span>
                  </div>
                  <span className="bcard-sub">{usd(src.totalUsd)}</span>
                </div>
                <div className="bcard-chain">
                  {g.chain}
                  {g.address ? (
                    <>
                      {" · "}
                      <span
                        className="copy"
                        title={g.address}
                        onClick={() =>
                          navigator.clipboard
                            ?.writeText(g.address!)
                            .catch(() => {})
                        }
                      >
                        {short(g.address)}
                      </span>
                    </>
                  ) : null}
                </div>
                {g.coins.length === 0 ? (
                  src.error ? null : (
                    <div className="bcard-empty">no balances</div>
                  )
                ) : (
                  g.coins.map((c, i) => (
                    <div key={i} className="bcoin">
                      <span className="a">{c.asset}</span>
                      <span className="n">
                        {fmt(c.free)}
                        {c.locked ? " +" + fmt(c.locked) : ""}
                      </span>
                      <span className="u">{usd(c.usd)}</span>
                    </div>
                  ))
                )}
                {src.error ? (
                  <div
                    className="bcard-empty"
                    style={{ color: "var(--bad)" }}
                  >
                    {src.error}
                  </div>
                ) : null}
              </div>
            );
          })
        )}
      </div>

      <div className="note">
        CEX balances via signed read-only keys (Bybit / KuCoin). Canton wallet
        cards show party-id; on-ledger balances come from cloud-agent when
        wired. Auto-refresh every 60s.
      </div>
    </div>
  );
}
