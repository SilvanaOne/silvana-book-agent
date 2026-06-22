import Link from "next/link";

import { ExecutionMonitorPanel } from "./execution-monitor";

export const dynamic = "force-dynamic";

export default async function MonitorDetailPage(props: { params: Promise<{ jobId: string }> }) {
  const { jobId } = await props.params;

  return (
    <main>
      <h1>Execution monitor</h1>

      <p>
        Job <code>{jobId}</code>
        {" · "}
        <Link href="/audit">audit</Link>
      </p>

      <ExecutionMonitorPanel jobId={jobId} />
    </main>
  );
}
