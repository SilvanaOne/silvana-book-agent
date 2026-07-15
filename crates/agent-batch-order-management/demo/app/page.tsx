import Link from "next/link";

export const dynamic = "force-dynamic";

// DEMO build: no backend, no network. The health probe resolves in-process.
async function fetchHealth() {
  return { ok: true, mode: "demo", service: "batch-order-agent-api" } as const;
}

export default async function HomePage() {
  const apiUrl = "demo://in-process (standalone fixtures)";

  const health = await fetchHealth();

  return (
    <main>
      <header className="silv-home-hero">
        <div style={{ display: "flex", alignItems: "center", gap: "0.5rem", marginBottom: "1.25rem" }}>
          <span style={{ fontSize: "0.95rem", fontWeight: 600, opacity: 0.95 }}>Batch</span>
          <span
            aria-hidden
            style={{
              display: "inline-flex",
              height: "1.125rem",
              alignItems: "center",
              padding: "0 0.35rem",
              border: "1px solid rgba(255,255,255,0.9)",
              borderRadius: 4,
              fontSize: "0.7rem",
              fontWeight: 500,
            }}
          >
            OPS
          </span>
        </div>

        <h1>
          Batch <span className="silv-home-hero__accent">orderbook operations</span>
        </h1>

        <p className="silv-home-hero__lead">
          Privacy, control and operational transparency in the spirit of <strong>Silvana Book</strong>: portfolio, rebalance
          simulations, queue monitor and venue configuration audit — in a single interface tuned to an institutional rhythm.
        </p>

        <div className="silv-pill-row">
          <Link href="/portfolio" className="silv-btn silv-btn--primary">
            Portfolio
          </Link>
          <Link href="/rebalance" className="silv-btn silv-btn--ghost-dark">
            Rebalance
          </Link>
          <Link href="/venues" className="silv-btn silv-btn--ghost-dark">
            Venues
          </Link>
        </div>
      </header>

      <section className="silv-panel">
        <h2 style={{ marginTop: 0 }}>Quick start</h2>

        <p className="muted">
          Sections are reachable from the top menu. If <code>API_INTERNAL_KEY</code> is set, the server proxy{" "}
          <code>/api/backend/*</code> attaches a Bearer token to API requests. Base URL: <code>BACKEND_INTERNAL_URL</code> or{" "}
          <code>NEXT_PUBLIC_API_URL</code>.
        </p>

        <h2>Backend health</h2>

        <p className="muted">
          Probing <code>GET /</code> on <code>{apiUrl}</code>
        </p>

        {health ? (
          <pre className="json-block">{JSON.stringify(health, null, 2)}</pre>
        ) : (
          <p>
            API server did not respond. From the monorepo root: <code>npm run dev:api</code> (port <code>3000</code> by
            default); web is usually on <code>3001</code>.
          </p>
        )}
      </section>
    </main>
  );
}
