import Link from "next/link";

import { backendJson } from "@/lib/backend";
import { InfoTip } from "@/app/components/InfoTip";

export const dynamic = "force-dynamic";

type AuditLogRow = {
  id: string;
  actorId: string;
  action: string;
  entityType: string;
  entityId: string;
  payload: unknown;
  createdAt: string;
};

export default async function AuditPage(props: {
  searchParams?: Promise<{ limit?: string | string[] }>;
}) {
  const sp = (await props.searchParams) ?? {};
  const limitRaw = sp.limit;

  const limitParsed =
    typeof limitRaw === "string" && /^[0-9]+$/.test(limitRaw)
      ? Math.min(500, Math.max(1, Number(limitRaw)))
      : 120;

  const fetched = await backendJson<{ logs: AuditLogRow[] }>(`/api/audit/logs?limit=${limitParsed}`);

  if (!fetched.ok) {
    return (
      <main>
        <h1>Audit log</h1>
        <p className="err">HTTP {fetched.status}</p>
        <pre className="err-block">{fetched.body}</pre>
      </main>
    );
  }

  const logs = fetched.data.logs;

  return (
    <main>
      <h1>Audit log</h1>

      <p className="muted">
        Showing the last {limitParsed} records (<Link href={`/audit?limit=200`}>200</Link>).
      </p>

      <table className="data-table">
        <thead>
          <tr>
            <th>
              <InfoTip text="Time the record was written to audit_log (createdAt). UTC, ISO. Used for ordering and tracing the sequence of events.">
                Time
              </InfoTip>
            </th>
            <th>
              <InfoTip text="Who initiated the action: user:<id>, mcp:<agent_id>, system. For system calls without an actor — system or ∅.">
                Actor
              </InfoTip>
            </th>
            <th>
              <InfoTip text="Event name: rebalance_execute_enqueued, rebalance_preview, order_submitted, order_cancelled, etc.">
                Action
              </InfoTip>
            </th>
            <th>
              <InfoTip text="Entity the event refers to: entityType + entityId. For rebalance_job there is a direct link to the monitoring page.">
                Entity
              </InfoTip>
            </th>
          </tr>
        </thead>
        <tbody>
          {logs.map((l) => (
            <tr key={l.id}>
              <td>
                <code>{l.createdAt}</code>
              </td>
              <td>{l.actorId}</td>
              <td>{l.action}</td>
              <td>
                {l.entityType} · <code>{l.entityId}</code>
                {l.entityType === "rebalance_job" ? (
                  <>
                    {" "}
                    <Link href={`/monitor/${encodeURIComponent(l.entityId)}`}>(monitor)</Link>
                  </>
                ) : null}
                <details className="muted">
                  <summary>
                    <InfoTip text="JSON payload of the event: input parameters, result and context. For rebalance — may contain silvana_order_id and stream phase ids.">
                      payload
                    </InfoTip>
                  </summary>
                  <pre className="json-block">{JSON.stringify(l.payload, null, 2)}</pre>
                </details>
              </td>
            </tr>
          ))}
        </tbody>
      </table>

      <p>
        If <code>silvana_order_id</code> values appear in payload, they are visible in the JSON block (for stream phases see the worker).
      </p>
    </main>
  );
}
