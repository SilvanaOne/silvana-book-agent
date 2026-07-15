"use client";

import Link from "next/link";
import { useEffect, useRef, useState } from "react";

import { InfoTip } from "@/app/components/InfoTip";
import { withBasePath } from "@/lib/base-path";

type OrderRow = {
  orderId: string;
  venue?: string;
  venueOrderRef?: string | null;
  execProfile?: string | null;
  silvanaOrderId: string | null;
  side: string;
  type: string;
  market: string;
  price: string | null;
  qty: string;
  status: string;
  clientOrderRef: string | null;
  createdAt: string;
  updatedAt: string;
};

type BatchRow = {
  batchId: string;
  status: string;
  venue: string;
  market: string;
  totalOrders: number;
  submittedOrders: number;
  filledOrders: number;
  cancelledOrders: number;
  orders: OrderRow[];
};

type SettlementRow = {
  id: string;
  status: string | null;
  createdAt: string;
  payload: unknown;
};

type JobDetail = {
  jobId: string;
  portfolioId: string;
  status: string;
  mode: string;
  dryRun: boolean;
  requestedBy: string | null;
  input: unknown;
  output: unknown;
  startedAt: string | null;
  finishedAt: string | null;
  createdAt: string;
  batches: BatchRow[];
  settlements: SettlementRow[];
};

export function ExecutionMonitorPanel(props: Readonly<{ jobId: string }>) {
  const [snap, setSnap] = useState<JobDetail | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const ivRef = useRef<number | null>(null);

  useEffect(() => {
    let alive = true;

    function clearTicker() {
      if (ivRef.current) {
        window.clearInterval(ivRef.current);
        ivRef.current = null;
      }
    }

    async function tick() {
      try {
        const res = await fetch(withBasePath(`/api/backend/rebalance/jobs/${encodeURIComponent(props.jobId)}?detail=true`), {
          cache: "no-store",
        });
        const text = await res.text();
        if (!alive) return;
        if (!res.ok) {
          setErr(`${res.status}: ${text}`);
          setSnap(null);
          return;
        }

        const data = JSON.parse(text) as JobDetail;
        setErr(null);
        setSnap(data);

        if (data.status === "completed" || data.status === "failed") {
          clearTicker();
        }
      } catch (e) {
        if (!alive) return;
        setErr(e instanceof Error ? e.message : String(e));
      }
    }

    clearTicker();
    void tick();
    ivRef.current = window.setInterval(() => {
      void tick();
    }, 2500);

    return () => {
      alive = false;
      clearTicker();
    };
  }, [props.jobId]);

  const isTerminal = snap?.status === "completed" || snap?.status === "failed";

  return (
    <div className="stack">
      {!snap ? <p className="muted">Loading status…</p> : null}

      {snap ? (
        <p>
          Polling every ~2.5 s (stops on <code>completed/failed</code>). Current status{" "}
          <code>{snap.status}</code>
          {snap.dryRun ? (
            <>
              {" · "}
              <strong>dry run</strong>
            </>
          ) : null}
          {isTerminal ? <> · final</> : null}
        </p>
      ) : null}

      {!isTerminal ? (
        <p className="muted">Keep this tab open until the job reaches a terminal state.</p>
      ) : null}

      <p>
        <Link href="/monitor">« Back</Link>
        {" · "}
        Portfolio:{" "}
        {snap ? <Link href={`/portfolio?id=${encodeURIComponent(snap.portfolioId)}`}>{snap.portfolioId}</Link> : "—"}
      </p>

      {err ? <p className="err">{err}</p> : null}

      {!snap ? null : (
        <>
          <section>
            <h2>
              <InfoTip text="Basic job fields from the rebalance_job table: mode, dry-run flag, initiator and timestamps.">
                Meta
              </InfoTip>
            </h2>
            <ul className="kv">
              <li>
                <InfoTip text="Run mode: live (real execution), dry_run (no venue calls), or preview (calculation only).">
                  mode
                </InfoTip>{" "}
                <code>{snap.mode}</code>
              </li>
              <li>
                <InfoTip text="If true — the job was enqueued with dryRun=true: the pipeline runs but real submit/cancel calls do not reach venues.">
                  dry_run
                </InfoTip>{" "}
                <code>{String(snap.dryRun)}</code>
              </li>
              <li>
                <InfoTip text="Identifier of the command initiator (user_id, mcp:agent_id, etc.). ∅ — anonymous/system call without an actor.">
                  requested_by
                </InfoTip>{" "}
                <code>{snap.requestedBy ?? "∅"}</code>
              </li>
              <li>
                <InfoTip text="Time when the job record was created in the DB (UTC, ISO).">created</InfoTip>{" "}
                <code>{snap.createdAt}</code>
              </li>
              <li>
                <InfoTip text="Time when the worker picked up the job from BullMQ and started executing. A dash means it has not started yet.">
                  started
                </InfoTip>{" "}
                <code>{snap.startedAt ?? "—"}</code>
              </li>
              <li>
                <InfoTip text="Time when the job reached a terminal state (completed/failed). A dash means it is still in progress.">
                  finished
                </InfoTip>{" "}
                <code>{snap.finishedAt ?? "—"}</code>
              </li>
            </ul>
          </section>

          <section>
            <details open>
              <summary>
                <InfoTip text="Job input: portfolioId, targets, thresholdBps, dryRun. The payload that came in /rebalance/execute.">
                  input
                </InfoTip>
              </summary>
              <pre className="json-block">{JSON.stringify(snap.input, null, 2)}</pre>
            </details>

            <details open>
              <summary>
                <InfoTip text="Execution result: portfolio value, plannedOrders, batches, errors. Filled in as the worker progresses through the pipeline.">
                  output
                </InfoTip>
              </summary>
              <pre className="json-block">{JSON.stringify(snap.output, null, 2)}</pre>
            </details>
          </section>

          <section>
            <h2>
              <InfoTip text="Groups of orders by venue+market that the worker submitted as a batch. Each batch is a single adapter submit call.">
                Batches
              </InfoTip>
            </h2>

            {snap.batches.length === 0 ? (
              <p className="muted">No batches (still queued, preview job, or enqueue error).</p>
            ) : (
              snap.batches.map((b) => (
                <details key={b.batchId} open>
                  <summary>
                    <strong>{b.batchId}</strong>
                    {" · "}
                    {b.venue}
                    {" — "}
                    {b.market}
                    {" · "}
                    <code>{b.status}</code>
                    {" · "}
                    <InfoTip text="Per-batch order counters: submitted / filled / cancelled. total = b.totalOrders.">
                      sub {b.submittedOrders}/ filled {b.filledOrders}/ cx {b.cancelledOrders}
                    </InfoTip>
                  </summary>
                  <table className="data-table">
                    <thead>
                      <tr>
                        <th>
                          <InfoTip text="Order UUID in our DB (orderbook.order). Not the same as venueOrderRef.">
                            order id
                          </InfoTip>
                        </th>
                        <th>
                          <InfoTip text="Venue the order was submitted to. With EXECUTION_ROUTER_ENABLED=1 it is decided by the router; otherwise it is the persisted value.">
                            venue
                          </InfoTip>
                        </th>
                        <th>
                          <InfoTip text="Id returned by the venue after submit (exchange order id or RFQ quote).">
                            venueOrderRef
                          </InfoTip>
                        </th>
                        <th>
                          <InfoTip text="Execution profile for the router: book / rfq / block / otc.">
                            execProfile
                          </InfoTip>
                        </th>
                        <th>
                          <InfoTip text="Internal silvana_order_id, if the order went through the Silvana stream/RPC.">
                            silvana id
                          </InfoTip>
                        </th>
                        <th>
                          <InfoTip text="Lifecycle: pending → submitted → partially_filled → filled / cancelled / rejected.">
                            status
                          </InfoTip>
                        </th>
                        <th>
                          <InfoTip text="buy — buy the base asset; sell — sell.">side</InfoTip>
                        </th>
                        <th>
                          <InfoTip text="Quantity in units of the base asset.">qty</InfoTip>
                        </th>
                        <th>
                          <InfoTip text="Limit price in the quote currency. A dash means a market order.">
                            price
                          </InfoTip>
                        </th>
                      </tr>
                    </thead>
                    <tbody>
                      {b.orders.map((o) => (
                        <tr key={o.orderId}>
                          <td>
                            <code>{o.orderId}</code>
                            {o.clientOrderRef ? <div className="muted">{o.clientOrderRef}</div> : null}
                          </td>
                          <td>{o.venue ?? "—"} </td>
                          <td>{o.venueOrderRef ?? "—"} </td>
                          <td>{o.execProfile ?? "—"} </td>
                          <td>{o.silvanaOrderId ?? "—"} </td>
                          <td>{o.status}</td>
                          <td>{o.side}</td>
                          <td>{o.qty}</td>
                          <td>{o.price ?? "—"} </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </details>
              ))
            )}
          </section>

          <section>
            <h2>
              <InfoTip text="Latest stream/webhook messages from venues for this job. Records from the settlement_event table.">
                Settlements
              </InfoTip>{" "}
              (latest records from DB)
            </h2>

            {snap.settlements.length === 0 ? (
              <p className="muted">Empty.</p>
            ) : (
              <ul className="stack">
                {snap.settlements.slice(0, 40).map((s) => (
                  <li key={s.id}>
                    <strong>{s.createdAt}</strong>
                    {" · "}
                    {s.status ?? "∅"}
                    <details>
                      <summary>payload</summary>
                      <pre className="json-block">{JSON.stringify(s.payload, null, 2)}</pre>
                    </details>
                  </li>
                ))}
              </ul>
            )}
            {snap.settlements.length > 40 ? (
              <p className="muted">Showing the last 40 rows.</p>
            ) : null}
          </section>
        </>
      )}
    </div>
  );
}
