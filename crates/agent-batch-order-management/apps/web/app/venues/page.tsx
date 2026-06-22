import { backendJson } from "@/lib/backend";

import { InfoTip } from "@/app/components/InfoTip";
import { VenueCard } from "../components/VenueCard";

import { PreviewRouteCard } from "./preview-route-card";

type VenueRow = Readonly<{
  venue: string;
  label: string;
  configured: boolean;
  mode: string;
  notes: string[];
}>;

type VenuesPayload = Readonly<{
  checkedAt: string;
  executionRouterEnabled: boolean;
  venues: VenueRow[];
}>;

/** Venues status page (Stage 9 → GET `/api/venues/status`). */
export default async function VenuesPage() {
  const res = await backendJson<VenuesPayload>("/api/venues/status");

  return (
    <main>
      <h1>Venues</h1>

      <p className="muted" style={{ maxWidth: 720 }}>
        Adapter configuration summary, with no trading calls. The worker router is toggled by{" "}
        <code>EXECUTION_ROUTER_ENABLED</code> (see <code>.env.example</code>).
      </p>

      {!res.ok ? (
        <p className="err">
          Failed to load status (<code>{res.status}</code>): {res.body.slice(0, 500)}
        </p>
      ) : null}

      {res.ok ? (
        <div className="silv-venues-band">
          <div className="silv-venues-band__inner">
            <p style={{ marginTop: 0, fontSize: "0.9rem", opacity: 0.95 }}>
              <strong>
                <InfoTip text="Timestamp of the latest adapter configuration check by the API. No trading endpoints are touched — only .env parsing.">
                  Updated
                </InfoTip>
                :
              </strong>{" "}
              <time dateTime={res.data.checkedAt}>{res.data.checkedAt}</time>
              {" · "}
              <strong>
                <InfoTip text="Global router flag. true — worker selects venue via chooseVenue (execution-router). false — uses the persisted value from the order (rollback path).">
                  EXECUTION_ROUTER_ENABLED
                </InfoTip>
                =
              </strong>
              <code>{String(res.data.executionRouterEnabled)}</code>
            </p>

            <div className="silv-venues-grid">
              {res.data.venues.map((v) => (
                <VenueCard key={v.venue} {...v} />
              ))}
            </div>
          </div>
        </div>
      ) : null}

      <PreviewRouteCard />
    </main>
  );
}
