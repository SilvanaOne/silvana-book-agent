/**
 * Prefix a hardcoded public-asset path (e.g. "/silvana-logo.svg", "/venues/x.svg")
 * with the deploy basePath. Next.js auto-prefixes `_next/*` and <Link>, but NOT
 * plain `<img src="/…">` or CSS `url(/…)`, so those must be wrapped with this.
 */
export const BASE_PATH = process.env.NEXT_PUBLIC_BASE_PATH ?? "";

export function asset(path: string): string {
  return `${BASE_PATH}${path}`;
}
