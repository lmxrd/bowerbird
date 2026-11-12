import { describe, expect, it } from "vitest";
import {
  DecryptError,
  Encrypting,
  b64,
  openBackendSecret,
  sealBackendSecret,
  decodeRecovery,
  decryptBytes,
  decryptName,
  encodeRecovery,
  encryptBytes,
  encryptName,
  encryptedSize,
  fileKey,
  hex,
  importPrivate,
  nameGeneration,
  nameKey,
  passphraseKey,
  plainSize,
  publicKey,
  recoveryWrappingKey,
  toHex,
  unwrapFolderKey,
  unwrapPrivate,
  wrapFolderKey,
  wrapPrivate,
  type Bytes,
} from "@/lib/e2e";

/** The reference implementation's vectors (packages/e2e-crypto, docs/E2E_SPEC.md §9). */
type Vectors = {
  format_version: number;
  identities: { private: string; public: string }[];
  passphrases: {
    passphrase: string;
    salt: string;
    iterations: number;
    key: string;
    private: string;
    nonce: string;
    wrapped: string;
  }[];
  recovery_keys: {
    key: string;
    text: string;
    wrapping_key: string;
    private: string;
    nonce: string;
    wrapped: string;
  }[];
  folder_keys: {
    folder_key: string;
    folder_id: string;
    generation: number;
    recipient_private: string;
    recipient_public: string;
    ephemeral_private: string;
    nonce: string;
    wrapped: string;
  }[];
  names: {
    folder_key: string;
    generation: number;
    parent_id: string;
    name: string;
    decrypted: string;
    name_key: string;
    nonce: string;
    encrypted: string;
  }[];
  backend_secrets: {
    secret: string;
    owner_id: string;
    recipient_private: string;
    recipient_public: string;
    ephemeral_private: string;
    nonce: string;
    sealed: string;
    sealed_base64url: string;
  }[];
  contents: {
    folder_key: string;
    generation: number;
    file_nonce: string;
    file_key: string;
    plain_length: number;
    encrypted_length: number;
    encrypted_sha256: string;
    encrypted?: string;
  }[];
};

const vectors = Object.values(
  import.meta.glob<Vectors>("../../../e2e-crypto/vectors.json", {
    eager: true,
    import: "default",
  }),
)[0];

const pattern = (n: number): Bytes => {
  const out = new Uint8Array(new ArrayBuffer(n));
  for (let i = 0; i < n; i++) out[i] = i % 251;
  return out;
};

const sha256 = async (value: Bytes) =>
  toHex(new Uint8Array(await crypto.subtle.digest("SHA-256", value)));

describe("the encryption vectors", () => {
  it("are of this format", () => {
    expect(vectors.format_version).toBe(1);
  });

  it("give each private key its public key", async () => {
    for (const id of vectors.identities) {
      expect(toHex(await publicKey(hex(id.private)))).toBe(id.public);
    }
  });

  it("derive the passphrase key and unwrap the private key", async () => {
    for (const p of vectors.passphrases) {
      const key = await passphraseKey(p.passphrase, hex(p.salt), p.iterations);
      expect(toHex(key)).toBe(p.key);
      expect(toHex(await wrapPrivate(key, hex(p.private), hex(p.nonce)))).toBe(p.wrapped);
      expect(toHex(await unwrapPrivate(key, hex(p.wrapped)))).toBe(p.private);
      const wrong = await passphraseKey(`${p.passphrase}!`, hex(p.salt), p.iterations);
      await expect(unwrapPrivate(wrong, hex(p.wrapped))).rejects.toThrow(DecryptError);
    }
  });

  it("read the recovery key and unwrap with it", async () => {
    for (const r of vectors.recovery_keys) {
      expect(encodeRecovery(hex(r.key))).toBe(r.text);
      expect(toHex(decodeRecovery(r.text.toLowerCase().replace(/-/g, " ")))).toBe(r.key);
      const wrapping = await recoveryWrappingKey(hex(r.key));
      expect(toHex(wrapping)).toBe(r.wrapping_key);
      expect(toHex(await wrapPrivate(wrapping, hex(r.private), hex(r.nonce)))).toBe(r.wrapped);
      expect(toHex(await unwrapPrivate(wrapping, hex(r.wrapped)))).toBe(r.private);
      // A typo in the last character (it carries one bit: A or Q).
      expect(() => decodeRecovery(`${r.text.slice(0, -1)}B`)).toThrow(DecryptError);
      expect(() => decodeRecovery(r.text.slice(1))).toThrow(DecryptError);
    }
  });

  it("wrap and unwrap folder keys", async () => {
    for (const f of vectors.folder_keys) {
      const wrapped = await wrapFolderKey(
        hex(f.folder_key),
        f.folder_id,
        f.generation,
        hex(f.recipient_public),
        hex(f.ephemeral_private),
        hex(f.nonce),
      );
      expect(toHex(wrapped)).toBe(f.wrapped);
      const privateKey = await importPrivate(hex(f.recipient_private));
      const key = await unwrapFolderKey(
        hex(f.wrapped),
        f.folder_id,
        f.generation,
        privateKey,
        hex(f.recipient_public),
      );
      expect(toHex(key)).toBe(f.folder_key);
      await expect(
        unwrapFolderKey(
          hex(f.wrapped),
          f.folder_id,
          f.generation + 1,
          privateKey,
          hex(f.recipient_public),
        ),
      ).rejects.toThrow(DecryptError);
    }
  });

  it("seal backends' secrets for their owner only", async () => {
    expect(vectors.backend_secrets.length).toBeGreaterThan(0);
    for (const s of vectors.backend_secrets) {
      const sealed = await sealBackendSecret(
        s.secret,
        s.owner_id,
        hex(s.recipient_public),
        hex(s.ephemeral_private),
        hex(s.nonce),
      );
      expect(toHex(sealed)).toBe(s.sealed);
      expect(b64(sealed)).toBe(s.sealed_base64url);
      const privateKey = await importPrivate(hex(s.recipient_private));
      expect(
        await openBackendSecret(hex(s.sealed), s.owner_id, privateKey, hex(s.recipient_public)),
      ).toBe(s.secret);
      await expect(
        openBackendSecret(
          hex(s.sealed),
          "00000000-0000-0000-0000-000000000000",
          privateKey,
          hex(s.recipient_public),
        ),
      ).rejects.toThrow(DecryptError);
    }
  });

  it("seal names for their folder only", async () => {
    for (const n of vectors.names) {
      const key = hex(n.folder_key);
      expect(toHex(await nameKey(key))).toBe(n.name_key);
      expect(await encryptName(key, n.generation, n.parent_id, n.name, hex(n.nonce))).toBe(
        n.encrypted,
      );
      expect(nameGeneration(n.encrypted)).toBe(n.generation);
      expect(await decryptName(key, n.parent_id, n.encrypted)).toBe(n.decrypted);
      await expect(
        decryptName(key, "00000000-0000-0000-0000-000000000000", n.encrypted),
      ).rejects.toThrow(DecryptError);
    }
  });

  it("encrypt contents in segments, and refuse them cut", async () => {
    for (const c of vectors.contents) {
      const key = hex(c.folder_key);
      expect(toHex(await fileKey(key, hex(c.file_nonce)))).toBe(c.file_key);
      const plain = pattern(c.plain_length);
      const sealed = await encryptBytes(key, c.generation, plain, hex(c.file_nonce));
      expect(sealed.length).toBe(c.encrypted_length);
      expect(encryptedSize(c.plain_length)).toBe(c.encrypted_length);
      expect(plainSize(c.encrypted_length)).toBe(c.plain_length);
      expect(await sha256(sealed)).toBe(c.encrypted_sha256);
      if (c.encrypted) expect(toHex(sealed)).toBe(c.encrypted);
      const keyOf = async (g: number) => (g === c.generation ? key : undefined);
      expect(toHex(await decryptBytes(keyOf, sealed))).toBe(toHex(plain));
      if (c.plain_length > 65_536) {
        // Cut after a full segment: it was not the last.
        await expect(decryptBytes(keyOf, sealed.slice(0, 37 + 65_552))).rejects.toThrow(
          DecryptError,
        );
      }
      await expect(decryptBytes(async () => undefined, sealed)).rejects.toThrow(DecryptError);
    }
  });

  it("give any range of an encrypted file on its own", async () => {
    const c = vectors.contents[3];
    const plain = pattern(c.plain_length);
    const encrypting = new Encrypting(
      hex(c.folder_key),
      c.generation,
      plain.length,
      async (s, e) => plain.slice(s, e),
      hex(c.file_nonce),
    );
    const whole = await encrypting.slice(0, encrypting.size);
    for (const [s, e] of [
      [0, 10],
      [30, 70_000],
      [65_589, 131_200],
      [100_000, encrypting.size],
    ]) {
      expect(toHex(await encrypting.slice(s, e))).toBe(toHex(whole.slice(s, e)));
    }
  });
});
