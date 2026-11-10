/**
 * Hybrid RSA-OAEP + AES-256-GCM payload envelope — port of the accounting
 * frontend's rsaEncrypt.ts, extended with the matching server-side decrypt.
 *
 * Credentials (login, password change) are encrypted in the browser with the
 * deployment's RSA public key before they touch the wire — defense in depth on
 * top of TLS (a TLS-terminating proxy or a verbose request log never sees a
 * raw password).
 *
 *   kid: key id — first 12 hex chars of sha256(public key PEM)
 *   k:   base64(RSA-OAEP ciphertext of base64(32-byte AES-256 key))
 *   i:   base64(12-byte AES-GCM IV)
 *   d:   base64(AES-256-GCM ciphertext || 16-byte auth tag)
 *
 * Isomorphic: WebCrypto only — encrypt runs in the browser, decrypt in
 * workerd/Node.
 *
 * This copy keeps the pure crypto (`encryptPayload` + the tested
 * `decryptPayload`/`payloadFresh`). The env-reading server config
 * (`rsaEnabled`, `rsa*KeyPem`) lives in the Rust API now.
 */
export interface HybridEnvelope {
  kid: string;
  k: string;
  i: string;
  d: string;
}

const PAYLOAD_MAX_AGE_MS = 5 * 60 * 1000;

function b64encode(bytes: Uint8Array | ArrayBuffer): string {
  const arr = bytes instanceof Uint8Array ? bytes : new Uint8Array(bytes);
  let binary = "";
  for (const b of arr) binary += String.fromCharCode(b);
  return btoa(binary);
}

function b64decode(b64: string): Uint8Array {
  return Uint8Array.from(atob(b64), (c) => c.charCodeAt(0));
}

function pemToDer(pem: string, label: string): ArrayBuffer {
  const b64 = pem
    .replace(new RegExp(`-----BEGIN ${label}-----`, "g"), "")
    .replace(new RegExp(`-----END ${label}-----`, "g"), "")
    .replace(/\s/g, "");
  return b64decode(b64).buffer as ArrayBuffer;
}

/** Client side: encrypt a JSON string into the envelope for the wire. */
export async function encryptPayload(
  plainText: string,
  publicKeyPem: string,
  keyId: string
): Promise<string> {
  const aesKey = crypto.getRandomValues(new Uint8Array(32));
  const iv = crypto.getRandomValues(new Uint8Array(12));

  const aesCryptoKey = await crypto.subtle.importKey("raw", aesKey as BufferSource, { name: "AES-GCM" }, false, [
    "encrypt",
  ]);
  const encrypted = await crypto.subtle.encrypt(
    { name: "AES-GCM", iv: iv as BufferSource },
    aesCryptoKey,
    new TextEncoder().encode(plainText)
  );

  // RSA-OAEP wraps the base64-encoded AES key (mirrors the reference: the
  // server base64-decodes after RSA decryption to recover the raw key).
  const rsaPublicKey = await crypto.subtle.importKey(
    "spki",
    pemToDer(publicKeyPem, "PUBLIC KEY"),
    { name: "RSA-OAEP", hash: "SHA-256" },
    false,
    ["encrypt"]
  );
  const encKey = await crypto.subtle.encrypt(
    { name: "RSA-OAEP" },
    rsaPublicKey,
    new TextEncoder().encode(b64encode(aesKey))
  );

  const envelope: HybridEnvelope = {
    kid: keyId,
    k: b64encode(encKey),
    i: b64encode(iv),
    d: b64encode(encrypted),
  };
  return JSON.stringify(envelope);
}

/** Server side: open an envelope produced by {@link encryptPayload}. Returns null on any failure. */
export async function decryptPayload(envelopeJson: string, privateKeyPem: string): Promise<string | null> {
  try {
    const env0 = JSON.parse(envelopeJson) as HybridEnvelope;
    if (typeof env0.k !== "string" || typeof env0.i !== "string" || typeof env0.d !== "string") return null;

    const rsaPrivateKey = await crypto.subtle.importKey(
      "pkcs8",
      pemToDer(privateKeyPem, "PRIVATE KEY"),
      { name: "RSA-OAEP", hash: "SHA-256" },
      false,
      ["decrypt"]
    );
    const aesKeyB64 = new TextDecoder().decode(
      await crypto.subtle.decrypt({ name: "RSA-OAEP" }, rsaPrivateKey, b64decode(env0.k) as BufferSource)
    );

    const aesCryptoKey = await crypto.subtle.importKey(
      "raw",
      b64decode(aesKeyB64) as BufferSource,
      { name: "AES-GCM" },
      false,
      ["decrypt"]
    );
    const plain = await crypto.subtle.decrypt(
      { name: "AES-GCM", iv: b64decode(env0.i) as BufferSource },
      aesCryptoKey,
      b64decode(env0.d) as BufferSource
    );
    return new TextDecoder().decode(plain);
  } catch {
    return null;
  }
}

/** Stable key id — lets the client pin which public key it encrypted with. */
export async function rsaKeyId(publicKeyPem: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(publicKeyPem));
  return Array.from(new Uint8Array(digest))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("")
    .slice(0, 12);
}

/** Freshness check on the `ts` field inside encrypted credential payloads. */
export function payloadFresh(ts: unknown): boolean {
  return typeof ts === "number" && Number.isFinite(ts) && Math.abs(Date.now() - ts) <= PAYLOAD_MAX_AGE_MS;
}
