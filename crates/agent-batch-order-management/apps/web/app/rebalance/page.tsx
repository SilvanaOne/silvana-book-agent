import Link from "next/link";

import { RebalanceBuilder } from "./rebalance-builder";

import { DEV_PORTFOLIO_ID } from "@/lib/dev-portfolio";

export const dynamic = "force-dynamic";

export default async function RebalancePage(props: {
  searchParams?: Promise<{ portfolioId?: string | string[] }>;
}) {
  const sp = (await props.searchParams) ?? {};
  const raw = sp.portfolioId;
  const portfolioId =
    typeof raw === "string" && raw.trim().length > 0 ? raw.trim() : DEV_PORTFOLIO_ID;

  return (
    <main>
      <h1>Rebalance builder</h1>

      <p className="muted">
        Run <strong>Simulate</strong> first; live execution is allowed only when risks pass. Default portfolio —{" "}
        <Link href={`/portfolio?id=${encodeURIComponent(DEV_PORTFOLIO_ID)}`}>seed UUID</Link>.
      </p>

      <div className="silv-panel">
        <RebalanceBuilder initialPortfolioId={portfolioId} />
      </div>
    </main>
  );
}
