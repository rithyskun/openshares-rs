import { describe, expect, it } from "vitest";
import crypto from "node:crypto";
import { decryptPayload, encryptPayload, payloadFresh } from "./envelope";

const { publicKey, privateKey } = crypto.generateKeyPairSync("rsa", {
  modulusLength: 2048,
  publicKeyEncoding: { type: "spki", format: "pem" },
  privateKeyEncoding: { type: "pkcs8", format: "pem" },
});

describe("hybrid envelope round-trip", () => {
  it("decrypts what encryptPayload produced", async () => {
    const inner = JSON.stringify({ email: "a@b.c", password: "hunter2", ts: Date.now() });
    const envelope = await encryptPayload(inner, publicKey, "kid-1");
    // Wire format: {kid,k,i,d} — never contains the plaintext.
    expect(envelope).not.toContain("hunter2");
    expect(JSON.parse(envelope)).toHaveProperty("kid", "kid-1");
    const plain = await decryptPayload(envelope, privateKey);
    expect(plain).toBe(inner);
  });

  it("returns null on a tampered envelope", async () => {
    const envelope = await encryptPayload('{"a":1}', publicKey, "kid-1");
    const parsed = JSON.parse(envelope) as { d: string };
    parsed.d = parsed.d.slice(0, -4) + "AAAA";
    expect(await decryptPayload(JSON.stringify(parsed), privateKey)).toBeNull();
  });

  it("returns null on garbage input", async () => {
    expect(await decryptPayload("not json", privateKey)).toBeNull();
    expect(await decryptPayload("{}", privateKey)).toBeNull();
  });
});

describe("payloadFresh", () => {
  it("accepts recent timestamps, rejects stale/non-numeric", () => {
    expect(payloadFresh(Date.now())).toBe(true);
    expect(payloadFresh(Date.now() - 10 * 60 * 1000)).toBe(false);
    expect(payloadFresh("soon")).toBe(false);
    expect(payloadFresh(undefined)).toBe(false);
  });
});
