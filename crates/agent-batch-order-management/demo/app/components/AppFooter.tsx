import Link from "next/link";

export function AppFooter() {
  return (
    <footer className="app-footer">
      <div className="app-footer__inner">
        <span>UI grammar — see <code>task/ui-styles.md</code> (Silvana&nbsp;Book&nbsp;promo)</span>

        <div style={{ display: "flex", flexWrap: "wrap", gap: "0.85rem", alignItems: "center" }}>
          <Link href="https://silvana.one/products/silvana_book" target="_blank" rel="noopener noreferrer">
            Silvana Book
          </Link>
          <Link href="https://docs.silvana.one" target="_blank" rel="noopener noreferrer">
            Docs
          </Link>
          <Link href="https://app.silvana.one" target="_blank" rel="noopener noreferrer">
            App
          </Link>
        </div>
      </div>
    </footer>
  );
}
