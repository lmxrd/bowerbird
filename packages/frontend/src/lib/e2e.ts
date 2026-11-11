/**
 * End-to-end encryption's format (docs/E2E_SPEC.md) on WebCrypto: X25519,
 * HKDF-SHA256, AES-256-GCM and PBKDF2-HMAC-SHA256. Pure functions, checked
 * against the reference's vectors (`e2e.test.ts`); what is kept while the
 * app runs (the unlocked key, the folders' keys) is `encryption.ts`.
 */

/** Bytes WebCrypto takes as they are. */
export type Bytes = Uint8Array<ArrayBuffer>;

/** The format's version: the first byte of everything written. */
export const VERSION = 1;
/** PBKDF2 iterations of a new passphrase; a reader takes the stored count. */
export const ITERATIONS = 600_000;
/** The fewest a reader accepts. */
export const MIN_ITERATIONS = 100_000;
/** The plaintext of a segment; the last has 0 to this many bytes. */
export const SEGMENT = 65_536;
const TAG = 16;
/** A sealed segment that is not the last. */
export const SEALED_SEGMENT = SEGMENT + TAG;
/** Version, generation, file nonce. */
export const HEADER = 1 + 4 + 32;
/** The most UTF-8 bytes of a name inside an encrypted folder. */
export const MAX_NAME_BYTES = 158;

const PRIVATE_KEY_AD = "organizer-e2e/1/private-key";
const RECOVERY_INFO = "organizer-e2e/1/recovery";
const WRAP_INFO = "organizer-e2e/1/folder-key";
const NAME_INFO = "organizer-e2e/1/name";
const FILE_INFO = "organizer-e2e/1/file";
const SECRET_INFO = "organizer-e2e/1/backend-secret";

const subtle = () => globalThis.crypto.subtle;
const text = new TextEncoder();

/** Why something cannot be read: a wrong key, or changed bytes. */
export class DecryptError extends Error {}

export function bytes(n: number): Bytes {
  return new Uint8Array(new ArrayBuffer(n));
}

export function random(n: number): Bytes {
  const out = bytes(n);
  globalThis.crypto.getRandomValues(out);
  return out;
}

export function concat(...pieces: (Uint8Array | number[])[]): Bytes {
  const parts = pieces.map((p) => (p instanceof Uint8Array ? p : Uint8Array.from(p)));
  const out = bytes(parts.reduce((n, p) => n + p.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

function copy(from: Uint8Array): Bytes {
  return concat(from);
}

export function utf8(value: string): Bytes {
  return copy(text.encode(value));
}

export function hex(value: string): Bytes {
  const out = bytes(value.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(value.slice(2 * i, 2 * i + 2), 16);
  return out;
}

export function toHex(value: Uint8Array): string {
  return Array.from(value, (b) => b.toString(16).padStart(2, "0")).join("");
}

/** Unpadded base64url, the API's encoding of bytes. */
export function b64(value: Uint8Array): string {
  let binary = "";
  for (const b of value) binary += String.fromCharCode(b);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export function unb64(value: string): Bytes {
  if (!/^[A-Za-z0-9_-]*$/.test(value) || value.length % 4 === 1) {
    throw new DecryptError("Not base64url");
  }
  const binary = atob(value.replace(/-/g, "+").replace(/_/g, "/"));
  const out = bytes(binary.length);
  for (let i = 0; i < binary.length; i++) out[i] = binary.charCodeAt(i);
  return out;
}

/** A UUID's 16 bytes. */
export function uuidBytes(id: string): Bytes {
  return hex(id.replace(/-/g, ""));
}

function u32(n: number): Bytes {
  const out = bytes(4);
  new DataView(out.buffer).setUint32(0, n);
  return out;
}

function readU32(from: Uint8Array, at: number): number {
  return new DataView(from.buffer, from.byteOffset, from.byteLength).getUint32(at);
}

/** HKDF-SHA256 to 32 bytes. */
export async function hkdf(ikm: Bytes, salt: Bytes, info: string): Promise<Bytes> {
  const key = await subtle().importKey("raw", ikm, "HKDF", false, ["deriveBits"]);
  const okm = await subtle().deriveBits(
    { name: "HKDF", hash: "SHA-256", salt, info: utf8(info) },
    key,
    256,
  );
  return new Uint8Array(okm);
}

async function aes(key: Bytes | CryptoKey): Promise<CryptoKey> {
  if (key instanceof CryptoKey) return key;
  return subtle().importKey("raw", key, "AES-GCM", false, ["encrypt", "decrypt"]);
}

/** AES-256-GCM: the ciphertext with its tag. */
export async function seal(
  key: Bytes | CryptoKey,
  nonce: Bytes,
  plain: Bytes,
  ad: Bytes = bytes(0),
): Promise<Bytes> {
  const sealed = await subtle().encrypt(
    { name: "AES-GCM", iv: nonce, additionalData: ad, tagLength: 128 },
    await aes(key),
    plain,
  );
  return new Uint8Array(sealed);
}

export async function open(
  key: Bytes | CryptoKey,
  nonce: Bytes,
  sealed: Bytes,
  ad: Bytes = bytes(0),
): Promise<Bytes> {
  try {
    const plain = await subtle().decrypt(
      { name: "AES-GCM", iv: nonce, additionalData: ad, tagLength: 128 },
      await aes(key),
      sealed,
    );
    return new Uint8Array(plain);
  } catch {
    throw new DecryptError("Cannot decrypt: a wrong key, or the data was changed");
  }
}

function checkVersion(value: Uint8Array) {
  if (value.length === 0) throw new DecryptError("Empty");
  if (value[0] !== VERSION) throw new DecryptError(`Format version ${value[0]} is not supported`);
}

// --- Identity keys ---------------------------------------------------------

/** PKCS #8 of an X25519 private key: this prefix and the 32 bytes. */
const PKCS8_PREFIX = hex("302e020100300506032b656e04220420");

/** The private key as a CryptoKey: not extractable unless asked. */
export async function importPrivate(raw: Bytes, extractable = false): Promise<CryptoKey> {
  return subtle().importKey("pkcs8", concat(PKCS8_PREFIX, raw), { name: "X25519" }, extractable, [
    "deriveBits",
  ]);
}

/** X25519's public key of a private key. */
export async function publicKey(raw: Bytes): Promise<Bytes> {
  const jwk = await subtle().exportKey("jwk", await importPrivate(raw, true));
  if (!jwk.x) throw new Error("This browser cannot work out an X25519 public key");
  return unb64(jwk.x);
}

/** X25519; an all-zero result (a public key of small order) is refused. */
export async function x25519(privateKey: CryptoKey, publicKeyBytes: Bytes): Promise<Bytes> {
  const peer = await subtle().importKey("raw", publicKeyBytes, { name: "X25519" }, true, []);
  const shared = new Uint8Array(
    await subtle().deriveBits({ name: "X25519", public: peer }, privateKey, 256),
  );
  if (shared.every((b) => b === 0)) throw new DecryptError("A public key of small order");
  return shared;
}

/** PBKDF2-HMAC-SHA256 of the passphrase's NFC form, 32 bytes. */
export async function passphraseKey(
  passphrase: string,
  salt: Bytes,
  iterations: number,
): Promise<Bytes> {
  const key = await subtle().importKey("raw", utf8(passphrase.normalize("NFC")), "PBKDF2", false, [
    "deriveBits",
  ]);
  const bits = await subtle().deriveBits(
    { name: "PBKDF2", hash: "SHA-256", salt, iterations },
    key,
    256,
  );
  return new Uint8Array(bits);
}

/** The private key wrapped under a wrapping key: version, nonce, sealed. */
export async function wrapPrivate(wrappingKey: Bytes, privateKey: Bytes, nonce = random(12)) {
  return concat([VERSION], nonce, await seal(wrappingKey, nonce, privateKey, utf8(PRIVATE_KEY_AD)));
}

export async function unwrapPrivate(wrappingKey: Bytes, wrapped: Bytes): Promise<Bytes> {
  checkVersion(wrapped);
  if (wrapped.length !== 61) throw new DecryptError("A wrapped private key is 61 bytes");
  return open(wrappingKey, wrapped.slice(1, 13), wrapped.slice(13), utf8(PRIVATE_KEY_AD));
}

// --- The recovery key ------------------------------------------------------

const BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/** As shown: 52 base32 characters in 13 groups of 4. */
export function encodeRecovery(key: Uint8Array): string {
  let out = "";
  let buffer = 0;
  let bits = 0;
  for (const b of key) {
    buffer = (buffer << 8) | b;
    bits += 8;
    while (bits >= 5) {
      out += BASE32[(buffer >> (bits - 5)) & 31];
      bits -= 5;
    }
  }
  if (bits > 0) out += BASE32[(buffer << (5 - bits)) & 31];
  return out.match(/.{1,4}/g)!.join("-");
}

/** As typed: dashes and spaces ignored, any case; anything else refused. */
export function decodeRecovery(typed: string): Bytes {
  const clean = typed.replace(/[-\s]/g, "").toUpperCase();
  if (clean.length !== 52 || [...clean].some((c) => !BASE32.includes(c))) {
    throw new DecryptError("A recovery key is 52 letters and digits");
  }
  const out = bytes(32);
  let buffer = 0;
  let bits = 0;
  let at = 0;
  for (const c of clean) {
    buffer = (buffer << 5) | BASE32.indexOf(c);
    bits += 5;
    if (bits >= 8 && at < 32) {
      out[at++] = (buffer >> (bits - 8)) & 255;
      bits -= 8;
    }
    buffer &= (1 << bits) - 1;
  }
  if (encodeRecovery(out).replace(/-/g, "") !== clean) {
    throw new DecryptError("That is not a recovery key");
  }
  return out;
}

export function recoveryWrappingKey(recovery: Bytes): Promise<Bytes> {
  return hkdf(recovery, bytes(0), RECOVERY_INFO);
}

// --- Folder keys -----------------------------------------------------------

function wrapAd(folderId: string, generation: number): Bytes {
  return concat(uuidBytes(folderId), u32(generation));
}

/**
 * Sealed to a public key (§4): an ephemeral X25519 agreement, HKDF-SHA256
 * salted with both public keys, AES-256-GCM. A folder key's wrapping and a
 * backend's secret (§11) differ in `info`, `ad` and what is sealed.
 */
async function sealTo(
  plain: Bytes,
  recipient: Bytes,
  info: string,
  ad: Bytes,
  ephemeral: Bytes,
  nonce: Bytes,
): Promise<Bytes> {
  const ephemeralKey = await importPrivate(ephemeral);
  const ephemeralPublic = await publicKey(ephemeral);
  const shared = await x25519(ephemeralKey, recipient);
  const key = await hkdf(shared, concat(ephemeralPublic, recipient), info);
  return concat([VERSION], ephemeralPublic, nonce, await seal(key, nonce, plain, ad));
}

/** What `sealTo` sealed, with the holder's keys. */
async function openSealed(
  sealed: Bytes,
  info: string,
  ad: Bytes,
  privateKey: CryptoKey,
  ownPublic: Bytes,
): Promise<Bytes> {
  checkVersion(sealed);
  const ephemeralPublic = sealed.slice(1, 33);
  const shared = await x25519(privateKey, ephemeralPublic);
  const key = await hkdf(shared, concat(ephemeralPublic, ownPublic), info);
  return open(key, sealed.slice(33, 45), sealed.slice(45), ad);
}

/** A folder key wrapped for a member's public key. */
export function wrapFolderKey(
  folderKey: Bytes,
  folderId: string,
  generation: number,
  recipient: Bytes,
  ephemeral = random(32),
  nonce = random(12),
): Promise<Bytes> {
  return sealTo(folderKey, recipient, WRAP_INFO, wrapAd(folderId, generation), ephemeral, nonce);
}

/** A folder key out of its wrapping, with the holder's keys. */
export async function unwrapFolderKey(
  wrapped: Bytes,
  folderId: string,
  generation: number,
  privateKey: CryptoKey,
  ownPublic: Bytes,
): Promise<Bytes> {
  checkVersion(wrapped);
  if (wrapped.length !== 93) throw new DecryptError("A wrapped folder key is 93 bytes");
  return openSealed(wrapped, WRAP_INFO, wrapAd(folderId, generation), privateKey, ownPublic);
}

// --- Backend secrets (§11) -------------------------------------------------

/** Version, ephemeral public key, nonce and tag around a sealed secret. */
export const SECRET_OVERHEAD = 1 + 32 + 12 + 16;

/** A storage backend's secret sealed to its owner's public key, bound to their id. */
export function sealBackendSecret(
  secret: string,
  ownerId: string,
  recipient: Bytes,
  ephemeral = random(32),
  nonce = random(12),
): Promise<Bytes> {
  return sealTo(utf8(secret), recipient, SECRET_INFO, uuidBytes(ownerId), ephemeral, nonce);
}

/** A backend's secret out of its sealing, with its owner's keys. */
export async function openBackendSecret(
  sealed: Bytes,
  ownerId: string,
  privateKey: CryptoKey,
  ownPublic: Bytes,
): Promise<string> {
  checkVersion(sealed);
  if (sealed.length < SECRET_OVERHEAD) throw new DecryptError("A sealed secret is too short");
  const plain = await openSealed(sealed, SECRET_INFO, uuidBytes(ownerId), privateKey, ownPublic);
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(plain);
  } catch {
    throw new DecryptError("A secret is UTF-8");
  }
}

// --- Names -----------------------------------------------------------------

export function nameKey(folderKey: Bytes): Promise<Bytes> {
  return hkdf(folderKey, bytes(0), NAME_INFO);
}

/** Why a name cannot be used in an encrypted folder, if it cannot. */
export function nameProblem(name: string): string | undefined {
  const nfc = name.normalize("NFC").trim();
  if (!nfc) return "A name is required";
  if (utf8(nfc).length > MAX_NAME_BYTES) return "That name is too long for an encrypted folder";
  return undefined;
}

/** A name sealed for its parent folder. */
export async function encryptName(
  folderKey: Bytes,
  generation: number,
  parentId: string,
  name: string,
  nonce = random(12),
): Promise<string> {
  const nfc = name.normalize("NFC");
  const problem = nameProblem(nfc);
  if (problem) throw new Error(problem);
  const sealed = await seal(await nameKey(folderKey), nonce, utf8(nfc), uuidBytes(parentId));
  return b64(concat([VERSION], u32(generation), nonce, sealed));
}

/** The generation of the key a name was sealed with. */
export function nameGeneration(encrypted: string): number {
  const raw = unb64(encrypted);
  checkVersion(raw);
  if (raw.length < 1 + 4 + 12 + 16) throw new DecryptError("Too short for a name");
  return readU32(raw, 1);
}

export async function decryptName(
  folderKey: Bytes,
  parentId: string,
  encrypted: string,
): Promise<string> {
  const raw = unb64(encrypted);
  checkVersion(raw);
  if (raw.length < 1 + 4 + 12 + 16) throw new DecryptError("Too short for a name");
  const plain = await open(
    await nameKey(folderKey),
    raw.slice(5, 17),
    raw.slice(17),
    uuidBytes(parentId),
  );
  return new TextDecoder("utf-8", { fatal: true }).decode(plain);
}

// --- Contents --------------------------------------------------------------

export function fileKey(folderKey: Bytes, fileNonce: Bytes): Promise<Bytes> {
  return hkdf(folderKey, fileNonce, FILE_INFO);
}

/** Seven zero bytes, the segment's number, 1 for the last. */
export function segmentNonce(index: number, last: boolean): Bytes {
  return concat(bytes(7), u32(index), [last ? 1 : 0]);
}

export function segments(plainSize: number): number {
  return Math.max(1, Math.ceil(plainSize / SEGMENT));
}

export function encryptedSize(plainSize: number): number {
  return HEADER + plainSize + TAG * segments(plainSize);
}

export function plainSize(size: number): number {
  const body = size - HEADER;
  if (body < TAG) throw new DecryptError("Shorter than a header and a segment");
  const full = Math.floor(body / SEALED_SEGMENT);
  const rest = body % SEALED_SEGMENT;
  if (rest === 0) return full * SEGMENT;
  if (rest < TAG) throw new DecryptError("A segment shorter than its tag");
  return full * SEGMENT + rest - TAG;
}

export function header(generation: number, fileNonce: Bytes): Bytes {
  return concat([VERSION], u32(generation), fileNonce);
}

export function readHeader(value: Uint8Array): { generation: number; fileNonce: Bytes } {
  if (value.length < HEADER) throw new DecryptError("Shorter than the header");
  checkVersion(value);
  return { generation: readU32(value, 1), fileNonce: copy(value.subarray(5, HEADER)) };
}

/**
 * A plaintext of `size` bytes, read a piece at a time (`read(start, end)`),
 * encrypted a segment at a time: `slice` gives any range of the encrypted
 * file without the rest, which is what a resumable upload asks for.
 */
export class Encrypting {
  readonly size: number;
  private key: Promise<CryptoKey>;
  private head: Bytes;

  constructor(
    folderKey: Bytes,
    generation: number,
    private plainLength: number,
    private read: (start: number, end: number) => Promise<Bytes>,
    fileNonce = random(32),
  ) {
    this.size = encryptedSize(plainLength);
    this.head = header(generation, fileNonce);
    this.key = fileKey(folderKey, fileNonce).then(aes);
  }

  /** Segment `index`, sealed. */
  async segment(index: number): Promise<Bytes> {
    const start = index * SEGMENT;
    const end = Math.min(start + SEGMENT, this.plainLength);
    const last = index === segments(this.plainLength) - 1;
    return seal(await this.key, segmentNonce(index, last), await this.read(start, end));
  }

  /** Bytes `start` to `end` of the encrypted file. */
  async slice(start: number, end: number): Promise<Bytes> {
    const parts: Uint8Array[] = [];
    if (start < HEADER) parts.push(this.head.subarray(start, Math.min(end, HEADER)));
    const from = Math.max(start, HEADER) - HEADER;
    const to = Math.max(end, HEADER) - HEADER;
    for (let i = Math.floor(from / SEALED_SEGMENT); i * SEALED_SEGMENT < to; i++) {
      const sealed = await this.segment(i);
      const base = i * SEALED_SEGMENT;
      parts.push(sealed.subarray(Math.max(from - base, 0), Math.min(to - base, sealed.length)));
    }
    return concat(...parts);
  }
}

/** A whole plaintext encrypted (the vectors, small files). */
export async function encryptBytes(
  folderKey: Bytes,
  generation: number,
  plain: Bytes,
  fileNonce = random(32),
): Promise<Bytes> {
  const encrypting = new Encrypting(
    folderKey,
    generation,
    plain.length,
    async (s, e) => plain.slice(s, e),
    fileNonce,
  );
  return encrypting.slice(0, encrypting.size);
}

/**
 * Decrypts a file as its bytes come (`push`), a segment at a time; `end`
 * checks that the last segment came. `keyOf` gives the folder key of a
 * generation.
 */
export class Decrypting {
  private buffered: Bytes = bytes(0);
  private key?: CryptoKey;
  private index = 0;
  private done = false;

  constructor(private keyOf: (generation: number) => Promise<Bytes | undefined>) {}

  /** The plaintext of the segments complete so far. */
  async push(chunk: Uint8Array): Promise<Bytes[]> {
    if (this.done) throw new DecryptError("Bytes after the last segment");
    this.buffered = concat(this.buffered, chunk);
    if (!this.key) {
      if (this.buffered.length < HEADER) return [];
      const { generation, fileNonce } = readHeader(this.buffered);
      const folderKey = await this.keyOf(generation);
      if (!folderKey) throw new DecryptError(`No key of generation ${generation}`);
      this.key = await aes(await fileKey(folderKey, fileNonce));
      this.buffered = this.buffered.slice(HEADER);
    }
    const out: Bytes[] = [];
    // A full segment is not the last while more follows it.
    while (this.buffered.length > SEALED_SEGMENT) {
      out.push(
        await open(
          this.key,
          segmentNonce(this.index++, false),
          this.buffered.slice(0, SEALED_SEGMENT),
        ),
      );
      this.buffered = this.buffered.slice(SEALED_SEGMENT);
    }
    return out;
  }

  /** The last segment's plaintext. */
  async end(): Promise<Bytes> {
    if (!this.key) throw new DecryptError("Shorter than the header");
    if (this.buffered.length < TAG) throw new DecryptError("A segment shorter than its tag");
    this.done = true;
    return open(this.key, segmentNonce(this.index, true), this.buffered);
  }
}

/** A whole encrypted file decrypted (the vectors, small files). */
export async function decryptBytes(
  keyOf: (generation: number) => Promise<Bytes | undefined>,
  encrypted: Bytes,
): Promise<Bytes> {
  const decrypting = new Decrypting(keyOf);
  const parts = await decrypting.push(encrypted);
  return concat(...parts, await decrypting.end());
}
