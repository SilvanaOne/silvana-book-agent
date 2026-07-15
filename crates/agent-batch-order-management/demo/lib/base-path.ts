export const BASE_PATH = process.env.NEXT_PUBLIC_BASE_PATH ?? "";
/** Prefix an absolute app path (e.g. "/api/backend/…") with the deploy basePath. */
export function withBasePath(p: string): string {
  return `${BASE_PATH}${p}`;
}
