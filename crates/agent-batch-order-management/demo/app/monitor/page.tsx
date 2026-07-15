import Link from "next/link";

import { DEV_PORTFOLIO_ID } from "@/lib/dev-portfolio";

import { MonitorJobLookup } from "./monitor-job-lookup";

export const dynamic = "force-dynamic";

export default async function MonitorIndexPage(props: {
  searchParams?: Promise<{ portfolioId?: string | string[] }>;
}) {
  const sp = (await props.searchParams) ?? {};
  const raw = sp.portfolioId;
  const portfolioIdDefault =
    typeof raw === "string" && raw.trim().length > 0 ? raw.trim() : DEV_PORTFOLIO_ID;

  return (
    <main>
      <h1>Execution monitor</h1>

      <p>
        After calling <code>execute</code>, follow the job id link (see <Link href="/rebalance">Rebalance builder</Link>).
      </p>

      <MonitorJobLookup defaultPortfolioId={portfolioIdDefault} />
    </main>
  );
}
