/**
 * Reusable skeleton for `loading.tsx` files. Shows a translucent placeholder
 * matching the page's heading + a row of fake cards / table lines, so navigation
 * feels instant even while the server is still preparing the real response.
 */
export function PageSkeleton(props: Readonly<{ heading: string; rows?: number }>) {
  const rows = props.rows ?? 6;
  return (
    <main aria-busy="true" aria-live="polite">
      <h1>{props.heading}</h1>
      <p className="muted">
        <span
          aria-hidden="true"
          style={{
            display: "inline-block",
            width: "0.9em",
            height: "0.9em",
            border: "2px solid currentColor",
            borderRightColor: "transparent",
            borderRadius: "50%",
            animation: "skel-spin 0.9s linear infinite",
            marginRight: "0.45em",
            verticalAlign: "-0.1em",
          }}
        />
        Loading…
      </p>

      <div
        style={{
          marginTop: "1.5rem",
          display: "grid",
          gap: "0.75rem",
        }}
      >
        {Array.from({ length: rows }).map((_, i) => (
          <div
            key={i}
            style={{
              height: "2.5rem",
              borderRadius: "0.5rem",
              background:
                "linear-gradient(90deg, var(--silv-skel-from, rgba(0,0,0,0.06)) 0%, var(--silv-skel-to, rgba(0,0,0,0.12)) 50%, var(--silv-skel-from, rgba(0,0,0,0.06)) 100%)",
              backgroundSize: "200% 100%",
              animation: "skel-shimmer 1.4s ease-in-out infinite",
              opacity: 1 - i * 0.08,
            }}
          />
        ))}
      </div>

      <style>{`
        @keyframes skel-spin { from { transform: rotate(0); } to { transform: rotate(360deg); } }
        @keyframes skel-shimmer {
          0% { background-position: 200% 0; }
          100% { background-position: -200% 0; }
        }
      `}</style>
    </main>
  );
}
