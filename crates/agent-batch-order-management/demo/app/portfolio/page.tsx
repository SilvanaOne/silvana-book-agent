import { redirect } from "next/navigation";

import { backendJson } from "@/lib/backend";
import { DEV_PORTFOLIO_ID } from "@/lib/dev-portfolio";
import { PortfolioLive, type PortfolioOverviewData } from "@/app/portfolio/portfolio-live";

export const dynamic = "force-dynamic";

type PortfolioListResponse = {
  portfolios: Array<{ id: string; name: string }>;
};

export default async function PortfolioOverviewPage(props: {
  searchParams?: Promise<{ id?: string | string[] }>;
}) {
  const sp = (await props.searchParams) ?? {};
  const raw = sp?.id;
  const explicitId = typeof raw === "string" && raw.trim().length > 0;
  const portfolioId = explicitId ? raw.trim() : DEV_PORTFOLIO_ID;

  const fetched = await backendJson<PortfolioOverviewData>(`/api/portfolio/${portfolioId}`);

  if (!fetched.ok && fetched.status === 404 && !explicitId) {
    const listed = await backendJson<PortfolioListResponse>("/api/portfolio");
    if (listed.ok && listed.data.portfolios.length > 0) {
      const first = listed.data.portfolios[0];
      if (first) {
        redirect(`/portfolio?id=${encodeURIComponent(first.id)}`);
      }
    }
  }

  if (!fetched.ok) {
    return (
      <main>
        <h1>Portfolio</h1>
        <p className="err">HTTP {fetched.status}</p>
        <pre className="err-block">{fetched.body}</pre>
        <p>
          {explicitId
            ? "This portfolio id is not in the database."
            : "No default portfolio and none found in the database."}{" "}
          Pass a UUID via <code>?id=</code> or run <code>npx prisma db seed</code> on the server.
        </p>
      </main>
    );
  }

  return <PortfolioLive initialData={fetched.data} />;
}
