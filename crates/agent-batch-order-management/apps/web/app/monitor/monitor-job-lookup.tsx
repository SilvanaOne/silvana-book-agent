"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";

import { InfoTip } from "@/app/components/InfoTip";

export function MonitorJobLookup(props: Readonly<{ defaultPortfolioId?: string }>) {
  const router = useRouter();
  const [jobId, setJobId] = useState("");

  return (
    <section className="silv-panel">
      <h2 style={{ marginTop: 0 }}>
        <InfoTip text="Open the details of a specific rebalance job. Use a UUID returned by /rebalance/execute or visible in the Audit log (entityType=rebalance_job).">
          Open job by ID
        </InfoTip>
      </h2>

      <label style={{ maxWidth: 520 }}>
        <InfoTip text="Job UUID from the /rebalance/execute response or from the audit_log table. Standard UUID format (8-4-4-4-12).">
          Job UUID
        </InfoTip>
        <input value={jobId} spellCheck={false} onChange={(e) => setJobId(e.target.value)} />
      </label>

      <div style={{ marginTop: "0.75rem" }}>
        <button
          type="button"
          className="silv-btn silv-btn--primary"
          disabled={!/^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(jobId.trim())}
          onClick={() => router.push(`/monitor/${encodeURIComponent(jobId.trim())}`)}
        >
          Open
        </button>
      </div>

      {props.defaultPortfolioId ? (
        <p className="muted">
          Default portfolio (seed):{" "}
          <Link href={`/portfolio?id=${encodeURIComponent(props.defaultPortfolioId)}`}>{props.defaultPortfolioId}</Link>
        </p>
      ) : null}
    </section>
  );
}
