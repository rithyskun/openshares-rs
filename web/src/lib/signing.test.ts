import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import {
  deriveSigningKey,
  requiresSignature,
  signRequest,
  verifySignature,
} from "./signing";

const KEY = "dGVzdC1rZXktMzItYnl0ZXMtbG9uZy1lbm91Z2gh"; // arbitrary test key

async function signAndVerify(
  method: string,
  path: string,
  body: string,
  tsOverride?: string
) {
  const sig = await signRequest(KEY, method, path, body);
  return verifySignature(
    {
      signature: sig["x-signature"],
      ts: tsOverride ?? sig["x-signature-ts"],
      nonce: sig["x-signature-nonce"],
    },
    KEY,
    method,
    path,
    body
  );
}

describe("requiresSignature", () => {
  it("covers mutating /api paths", () => {
    expect(requiresSignature("POST", "/api/albums")).toBe(true);
    expect(requiresSignature("DELETE", "/api/media/abc")).toBe(true);
    expect(requiresSignature("PATCH", "/api/admin/users/3")).toBe(true);
  });

  it("skips GET and non-API paths", () => {
    expect(requiresSignature("GET", "/api/albums")).toBe(false);
    expect(requiresSignature("POST", "/gallery")).toBe(false);
  });

  it("exempts auth bootstrap, bot, and multipart uploads", () => {
    expect(requiresSignature("POST", "/api/auth/login")).toBe(false);
    expect(requiresSignature("POST", "/api/bot/webhook")).toBe(false);
    expect(requiresSignature("POST", "/api/albums/abc/files")).toBe(false);
    // …but other album sub-paths are still signed
    expect(requiresSignature("PATCH", "/api/albums/abc")).toBe(true);
  });
});

describe("sign/verify round-trip", () => {
  it("accepts a fresh signature over the exact body", async () => {
    expect(await signAndVerify("POST", "/api/albums", '{"title":"x"}')).toBe("ok");
    expect(await signAndVerify("DELETE", "/api/media/abc", "")).toBe("ok");
  });

  it("rejects a tampered body", async () => {
    const sig = await signRequest(KEY, "PATCH", "/api/media/abc", '{"caption":"a"}');
    expect(
      await verifySignature(
        { signature: sig["x-signature"], ts: sig["x-signature-ts"], nonce: sig["x-signature-nonce"] },
        KEY,
        "PATCH",
        "/api/media/abc",
        '{"caption":"b"}'
      )
    ).toBe("bad");
  });

  it("rejects a signature for a different path", async () => {
    const sig = await signRequest(KEY, "DELETE", "/api/media/a", "");
    expect(
      await verifySignature(
        { signature: sig["x-signature"], ts: sig["x-signature-ts"], nonce: sig["x-signature-nonce"] },
        KEY,
        "DELETE",
        "/api/media/b",
        ""
      )
    ).toBe("bad");
  });

  it("rejects missing headers and stale timestamps", async () => {
    expect(
      await verifySignature({ signature: null, ts: null, nonce: null }, KEY, "POST", "/api/x", "")
    ).toBe("missing");
    const sig = await signRequest(KEY, "POST", "/api/x", "");
    expect(
      await verifySignature(
        {
          signature: sig["x-signature"],
          ts: String(Date.now() - 10 * 60 * 1000), // 10 min old
          nonce: sig["x-signature-nonce"],
        },
        KEY,
        "POST",
        "/api/x",
        ""
      )
    ).toBe("stale");
  });
});

describe("deriveSigningKey", () => {
  beforeEach(() => {
    vi.useRealTimers();
  });
  afterEach(() => {
    vi.unstubAllEnvs();
  });

  it("is deterministic per session and differs across sessions", async () => {
    vi.stubEnv("REQUEST_SIGNING_SECRET", "test-secret");
    const a1 = await deriveSigningKey("test-secret", "sid-a");
    const a2 = await deriveSigningKey("test-secret", "sid-a");
    const b = await deriveSigningKey("test-secret", "sid-b");
    expect(a1).toBe(a2);
    expect(a1).not.toBe(b);
    expect(a1).toMatch(/^[A-Za-z0-9+/=]+$/);
  });

  it("a key derived for one session fails on another's signature", async () => {
    const keyA = await deriveSigningKey("test-secret", "sid-a");
    const keyB = await deriveSigningKey("test-secret", "sid-b");
    const sig = await signRequest(keyA, "POST", "/api/x", "");
    expect(
      await verifySignature(
        { signature: sig["x-signature"], ts: sig["x-signature-ts"], nonce: sig["x-signature-nonce"] },
        keyB,
        "POST",
        "/api/x",
        ""
      )
    ).toBe("bad");
  });
});
