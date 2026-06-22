import { InfoTip } from "./InfoTip";

/** Venue card on `/venues` — Silvana Book style (card + accent). */
export type VenueStatusCardProps = Readonly<{
  venue: string;
  label: string;
  configured: boolean;
  mode: string;
  notes: string[];
}>;

export function VenueCard(props: VenueStatusCardProps) {
  const okCls = props.configured ? " silv-card--venue-ok" : " silv-card--venue-warn";

  return (
    <article className={`silv-card silv-card--venue${okCls}`}>
      <header className="silv-card__header">
        <div>
          <strong className="silv-card__title">{props.label}</strong>
          <div className="silv-card__sub">
            <InfoTip text="Internal adapter identifier (silvana, okx, ...). The worker and router use it to find the venue configuration.">
              id
            </InfoTip>
            : <code>{props.venue}</code>
          </div>
        </div>

        <span className={`silv-card__chip${props.configured ? " silv-card__chip--ok" : " silv-card__chip--no"}`}>
          <InfoTip
            text="configured=true means all required venue keys are set in .env (API keys/connectors). configured=false means the configuration is incomplete and trading calls will fail."
            align="end"
          >
            {props.configured ? "configured" : "not configured"}
          </InfoTip>
        </span>
      </header>

      <p className="silv-card__body">
        <strong>
          <InfoTip text="Adapter mode: enabled / dry-run / disabled. Configured via .env (e.g. <VENUE>_MODE).">
            mode
          </InfoTip>
          :
        </strong>{" "}
        <code>{props.mode}</code>
      </p>

      {props.notes?.length ? (
        <ul className="silv-card__notes">
          {props.notes.map((n, i) => (
            <li key={`${props.venue}:${i}:${n.slice(0, 32)}`}>{n}</li>
          ))}
        </ul>
      ) : null}
    </article>
  );
}
