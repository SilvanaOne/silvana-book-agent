export function pemToDer(pem: string): ArrayBuffer {
  const b64 = pem.replace(/-----[^-]+-----/g, "").replace(/\s+/g, "");
  const bin = atob(b64);
  const u = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) u[i] = bin.charCodeAt(i);
  return u.buffer;
}

export async function rsaEncrypt(
  key: CryptoKey | null,
  plaintext: string,
): Promise<string> {
  const bytes = new TextEncoder().encode(plaintext);
  if (!key) {
    // Demo fallback — if key isn't ready, still base64 the input so the
    // wire message shape matches the real vault.
    return btoa(String.fromCharCode(...bytes));
  }
  const ct = await crypto.subtle.encrypt({ name: "RSA-OAEP" }, key, bytes);
  return btoa(String.fromCharCode(...new Uint8Array(ct)));
}
