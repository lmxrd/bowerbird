/**
 * End-to-end encryption in this browser (docs/E2E_SPEC.md): the unlocked
 * private key, a non-extractable CryptoKey kept for this page's session
 * only (a reload locks again), the folders' keys unwrapped with it, and
 * what a device does with them: names and contents in and out of
 * encrypted folders, keys for members waiting, new keys when someone left.
 * The format itself is `e2e.ts`.
 *
 * In a mirror, and in a folder shared from another Organizer server
 * (docs/E2E_SPEC.md §12), an encrypted folder is sealed with the ids where
 * it comes from (`Node.source_id`): its keys with the answer's
 * `folder_id`, each name with its parent's source (`sealingId`).
 */

import { useSyncExternalStore } from "react";
import type { Backend, EncryptionKeys, FolderKeys, MemberKey, Node } from "organizer-sdk";
import {
  addFolderKeys,
  changePassphrase,
  getEncryption,
  getFile,
  getFolderKeys,
  listBackends,
  listChildren,
  listEncryptionTasks,
  lockBackend,
  setUpEncryption,
  unlockBackend,
} from "organizer-sdk";
import { errorCode, queryClient } from "@/lib/api";
import { MAX_SEALED } from "@/lib/backends";
import {
  type Bytes,
  DecryptError,
  Decrypting,
  Encrypting,
  ITERATIONS,
  MIN_ITERATIONS,
  b64,
  decodeRecovery,
  decryptName,
  encodeRecovery,
  encryptName,
  importPrivate,
  nameGeneration,
  openBackendSecret,
  passphraseKey,
  publicKey,
  random,
  recoveryWrappingKey,
  sealBackendSecret,
  unb64,
  unwrapFolderKey,
  unwrapPrivate,
  wrapFolderKey,
  wrapPrivate,
} from "@/lib/e2e";

type Unlocked = { privateKey: CryptoKey; publicKey: Bytes };

let unlocked: Unlocked | undefined;
/** Each encrypted folder's keys, by generation. */
const folderKeys = new Map<string, { newest: number; keys: Map<number, Bytes> }>();
const listeners = new Set<() => void>();

function changed() {
  for (const listener of listeners) listener();
}

/** Whether this browser holds the unlocked key: re-renders when it changes. */
export function useUnlocked(): boolean {
  return useSyncExternalStore(
    (listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    () => unlocked !== undefined,
  );
}

export function isUnlocked(): boolean {
  return unlocked !== undefined;
}

/** Forget the key and every folder key: locked again. */
export function lock() {
  unlocked = undefined;
  folderKeys.clear();
  changed();
  void lockBackends();
}

// --- Private credentials (docs/E2E_SPEC.md §11) ----------------------------

/**
 * The secret of the user's own storage sealed to their public key (which
 * needs encryption set up, not unlocked), as `sealed_secret` takes it.
 */
export async function sealBackendSecretForMe(secret: string, userId: string): Promise<string> {
  const keys = await keyMaterial();
  if (!keys.set_up || !keys.public_key) throw new Error("Set up encryption first");
  const sealed = b64(await sealBackendSecret(secret, userId, unb64(keys.public_key)));
  if (sealed.length > MAX_SEALED) throw new Error("That secret is too long to seal");
  return sealed;
}

/** Whether this browser can unlock a private backend by itself. */
export function canUnlock(backend: Backend): boolean {
  return !!backend.sealed_secret;
}

/** One private backend opened with the unlocked key and handed to the server for this session. */
export async function unlockBackendWithKey(backend: Backend) {
  const me = need();
  const sealed = backend.sealed_secret;
  const owner = backend.owner?.id;
  if (!sealed || !owner) throw new Error("This browser has no sealed secret for it");
  let secret: string;
  try {
    secret = await openBackendSecret(unb64(sealed), owner, me.privateKey, me.publicKey);
  } catch (error) {
    if (error instanceof DecryptError) throw new Error("Its secret doesn't open with your key");
    throw error;
  }
  await unlockBackend({ throwOnError: true, path: { id: backend.id }, body: { secret } });
}

/** Each of the user's locked private backends, unlocked once encryption is. */
export async function unlockBackends() {
  if (!unlocked) return;
  try {
    const backends = (await listBackends({ throwOnError: true })).data;
    for (const backend of backends) {
      if (!backend.private || backend.status !== "locked" || !canUnlock(backend)) continue;
      try {
        await unlockBackendWithKey(backend);
      } catch {
        // Its row says it is still locked, with the reason.
      }
    }
    void queryClient.invalidateQueries();
  } catch {
    // Not allowed own storage, or offline: nothing to unlock.
  }
}

/** This session's hold on the private backends given up: locked with encryption. */
async function lockBackends() {
  try {
    const backends = (await listBackends({ throwOnError: true })).data;
    for (const backend of backends) {
      if (backend.private && backend.status !== "locked") {
        await lockBackend({ path: { id: backend.id } });
      }
    }
    void queryClient.invalidateQueries();
  } catch {
    // Nothing held, or offline: the session's end drops it anyway.
  }
}

/** The user's keys as the server keeps them. */
export async function keyMaterial(): Promise<EncryptionKeys> {
  return (await getEncryption({ throwOnError: true })).data;
}

/** A passphrase that is too weak to protect the keys, if it is. */
export function passphraseProblem(passphrase: string, again: string): string | undefined {
  if (passphrase.length < 10) return "Use at least 10 characters";
  if (passphrase !== again) return "The passphrases don't match";
  return undefined;
}

async function keep(privateRaw: Bytes, expectedPublic: string) {
  if (b64(await publicKey(privateRaw)) !== expectedPublic) {
    throw new Error("These keys don't belong together: ask your superadmin");
  }
  unlocked = {
    privateKey: await importPrivate(privateRaw),
    publicKey: unb64(expectedPublic),
  };
  privateRaw.fill(0);
  changed();
  void seePending();
  void unlockBackends();
}

async function passphraseWrap(privateRaw: Bytes, passphrase: string) {
  const salt = random(16);
  const key = await passphraseKey(passphrase, salt, ITERATIONS);
  return {
    salt: b64(salt),
    iterations: ITERATIONS,
    wrapped: b64(await wrapPrivate(key, privateRaw)),
  };
}

/** The private key out of its passphrase wrapping. */
async function withPassphrase(keys: EncryptionKeys, passphrase: string): Promise<Bytes> {
  const wrap = keys.passphrase;
  if (!keys.set_up || !wrap) throw new Error("Encryption isn't set up");
  if (wrap.iterations < MIN_ITERATIONS) throw new Error("These keys are not safe to use");
  const key = await passphraseKey(passphrase, unb64(wrap.salt), wrap.iterations);
  try {
    return await unwrapPrivate(key, unb64(wrap.wrapped));
  } catch (error) {
    if (error instanceof DecryptError) throw new Error("That passphrase is not the right one");
    throw error;
  }
}

/** Unlock on this device with the passphrase. */
export async function unlock(passphrase: string) {
  const keys = await keyMaterial();
  await keep(await withPassphrase(keys, passphrase), keys.public_key ?? "");
}

/**
 * Set up encryption: a new key pair, wrapped under the passphrase and a new
 * recovery key, which is answered to be shown once. Unlocked afterwards.
 */
export async function setUp(passphrase: string): Promise<string> {
  const privateRaw = random(32);
  const recovery = random(32);
  const recoveryWrapped = await wrapPrivate(await recoveryWrappingKey(recovery), privateRaw);
  const publicB64 = b64(await publicKey(privateRaw));
  await setUpEncryption({
    throwOnError: true,
    body: {
      public_key: publicB64,
      passphrase: await passphraseWrap(privateRaw, passphrase),
      recovery_wrapped: b64(recoveryWrapped),
    },
  });
  const text = encodeRecovery(recovery);
  recovery.fill(0);
  await keep(privateRaw, publicB64);
  return text;
}

/** A new passphrase, with the current one. */
export async function changeToPassphrase(current: string, next: string) {
  const keys = await keyMaterial();
  const privateRaw = await withPassphrase(keys, current);
  await changePassphrase({ throwOnError: true, body: await passphraseWrap(privateRaw, next) });
  await keep(privateRaw, keys.public_key ?? "");
}

/** A forgotten passphrase replaced, with the recovery key. */
export async function recoverWith(recoveryText: string, next: string) {
  const keys = await keyMaterial();
  if (!keys.set_up || !keys.recovery_wrapped) throw new Error("Encryption isn't set up");
  let privateRaw: Bytes;
  try {
    const recovery = decodeRecovery(recoveryText);
    privateRaw = await unwrapPrivate(
      await recoveryWrappingKey(recovery),
      unb64(keys.recovery_wrapped),
    );
  } catch (error) {
    if (error instanceof DecryptError) throw new Error("That is not your recovery key");
    throw error;
  }
  await changePassphrase({ throwOnError: true, body: await passphraseWrap(privateRaw, next) });
  await keep(privateRaw, keys.public_key ?? "");
}

function need(): Unlocked {
  if (!unlocked) throw new Error("Unlock encryption on this device first");
  return unlocked;
}

/**
 * The keys of an answer, kept by the id they were asked by. They are
 * wrapped for the answer's `folder_id`: the folder itself, or where it
 * comes from (a mirror's drive, the owner's server).
 */
async function readKeys(root: string, data: FolderKeys) {
  const me = need();
  const keys = new Map<number, Bytes>();
  for (const key of data.keys) {
    keys.set(
      key.generation,
      await unwrapFolderKey(
        unb64(key.wrapped),
        data.folder_id,
        key.generation,
        me.privateKey,
        me.publicKey,
      ),
    );
  }
  folderKeys.set(root, { newest: data.generation, keys });
  return folderKeys.get(root)!;
}

/** Said when the server a folder is shared from does not answer for its keys. */
export const OWNER_UNREACHABLE = "The owner's server is not reachable. Try again later.";

/**
 * An encrypted folder's keys as the server answers them; a folder shared
 * from another server whose server does not answer (502
 * `remote_unreachable`) is said so.
 */
export async function fetchKeys(id: string): Promise<FolderKeys> {
  try {
    return (await getFolderKeys({ throwOnError: true, path: { id } })).data;
  } catch (error) {
    throw keysError(error);
  }
}

/** The error of asking for an encrypted folder's keys, in words where it has a code we know. */
export function keysError(error: unknown): unknown {
  return errorCode(error) === "remote_unreachable" ? new Error(OWNER_UNREACHABLE) : error;
}

/** An encrypted folder's keys (fetched once, or again with `fresh`). */
async function keysOf(root: string, fresh = false) {
  const known = folderKeys.get(root);
  if (known && !fresh) return known;
  return readKeys(root, await fetchKeys(root));
}

/** That an encrypted folder's keys can be had: the error why not, when not. */
export async function openKeys(root: string) {
  await keysOf(root);
}

/** The folder key of a generation, read again once if it is not known yet. */
async function keyOf(root: string, generation: number): Promise<Bytes | undefined> {
  return (
    (await keysOf(root)).keys.get(generation) ?? (await keysOf(root, true)).keys.get(generation)
  );
}

/** The newest key, for what is written now: made first if there is none. */
async function newestKey(root: string): Promise<{ key: Bytes; generation: number }> {
  let known = await keysOf(root, true);
  if (known.newest === 0 || !known.keys.has(known.newest)) {
    await seePending();
    known = await keysOf(root, true);
  }
  const key = known.keys.get(known.newest);
  if (!key) throw new Error("This folder has no key for you yet: ask its owner to open it");
  return { key, generation: known.newest };
}

/** What a node shows as its name: inside an encrypted folder, decrypted. */
export async function nameOf(node: Pick<Node, "id" | "name" | "parent_id" | "e2e_root">) {
  if (!node.e2e_root || node.e2e_root === node.id || !node.parent_id) return node.name;
  const key = await keyOf(node.e2e_root, nameGeneration(node.name));
  if (!key) throw new DecryptError("No key for this name");
  return decryptName(key, node.parent_id, node.name);
}

/** As `nameOf`, with a stand-in for what cannot be read. */
export async function shownName(node: Pick<Node, "id" | "name" | "parent_id" | "e2e_root">) {
  try {
    return await nameOf(node);
  } catch {
    return "Encrypted item";
  }
}

/** What the source of a folder of a copy is looked up by: asked of the server again. */
type Relist = (id: string) => Promise<Pick<Node, "source_id">>;

const relist: Relist = async (id) => (await getFile({ throwOnError: true, path: { id } })).data;

/**
 * The id the names in a folder of an encrypted folder are sealed with
 * (docs/E2E_SPEC.md §6, §12): its own; in a mirror and in a folder shared
 * from another server (`remote`), its `source_id`, its id where it comes
 * from. A folder made there a moment ago may have none yet: asked again
 * (the server fills it in once the other server has it), and refused while
 * it still has none, so nothing is sealed with an id the owner does not
 * know.
 */
export async function sealingId(
  folder: Pick<Node, "id" | "source_id" | "remote">,
  ask: Relist = relist,
): Promise<string> {
  if (folder.source_id) return folder.source_id;
  if (!folder.remote) return folder.id;
  const again = (await ask(folder.id)).source_id;
  if (!again) {
    throw new Error("This folder isn't on its owner's server yet. Try again in a moment.");
  }
  return again;
}

type Sealing = Pick<Node, "id" | "e2e_root" | "source_id" | "remote">;

/** Every name in a folder, decrypted (for clash checks and lookups). */
async function namesIn(folder: Sealing) {
  // Inside an encrypted folder, its children's names are sealed with its sealing id.
  const parent = folder.e2e_root ? await sealingId(folder) : folder.id;
  const out: { node: Node; name: string }[] = [];
  let after: string | undefined;
  do {
    const page = (
      await listChildren({
        throwOnError: true,
        path: { id: folder.id },
        query: { limit: 1000, ...(after ? { after } : {}) },
      })
    ).data;
    for (const node of page.items) {
      out.push({ node, name: await shownName({ ...node, parent_id: parent }) });
    }
    after = page.next ?? undefined;
  } while (after);
  return out;
}

/** A child of a folder by the name the user sees. */
export async function childNamed(folder: Sealing, name: string) {
  const wanted = name.normalize("NFC");
  return (await namesIn(folder)).find((c) => c.name === wanted)?.node;
}

/**
 * The name to store for `name` in `folder` (an encrypted folder or one in
 * it): sealed with the newest key for that parent (its sealing id), after
 * a check that no other item there has it, but `except`.
 */
export async function sealedName(folder: Sealing, name: string, except?: string): Promise<string> {
  const root = folder.e2e_root!;
  const folded = name.normalize("NFC").trim().toLowerCase();
  const clash = (await namesIn(folder)).find(
    (c) => c.node.id !== except && c.name.toLowerCase() === folded,
  );
  if (clash) throw new Error(`${clash.name} already exists`);
  const { key, generation } = await newestKey(root);
  return encryptName(key, generation, await sealingId(folder), name.trim());
}

/** A file to upload into an encrypted folder: its sealed name and its encrypted contents. */
export async function encryptedUpload(folder: Sealing, file: File) {
  const name = await sealedName(folder, file.name);
  const { key, generation } = await newestKey(folder.e2e_root!);
  const read = async (start: number, end: number) =>
    new Uint8Array(await file.slice(start, end).arrayBuffer());
  return { name, contents: new Encrypting(key, generation, file.size, read) };
}

/** A file of an encrypted folder, downloaded and decrypted as it comes. */
export async function decryptedFile(node: Node, url: string): Promise<Blob> {
  const root = node.e2e_root!;
  const response = await fetch(url);
  if (!response.ok || !response.body) throw new Error(`The download failed (${response.status})`);
  const decrypting = new Decrypting((generation) => keyOf(root, generation));
  const parts: Bytes[] = [];
  const reader = response.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    parts.push(...(await decrypting.push(value)));
  }
  parts.push(await decrypting.end());
  return new Blob(parts);
}

/**
 * The keys a member's device posts for an encrypted folder: with `fresh`, a
 * new generation for every member with a public key (the server refuses
 * one that leaves any out); and each generation `held` here for the
 * members who lack it. A member on another server (`remote`) is wrapped
 * for as any, by their `user_id`, which is the share's id: never looked up
 * as a user here.
 */
export async function keysToPost(
  data: Pick<FolderKeys, "folder_id" | "generation" | "members">,
  held: Map<number, Bytes>,
  fresh?: Bytes,
): Promise<MemberKey[]> {
  const keys: MemberKey[] = [];
  const wrapFor = async (userId: string, key: Bytes, generation: number, publicKey: string) => {
    const wrapped = await wrapFolderKey(key, data.folder_id, generation, unb64(publicKey));
    keys.push({ user_id: userId, generation, wrapped: b64(wrapped) });
  };
  for (const member of data.members) {
    if (!member.public_key) continue;
    if (fresh) await wrapFor(member.user_id, fresh, data.generation + 1, member.public_key);
    for (const generation of member.missing) {
      const key = held.get(generation);
      if (key) await wrapFor(member.user_id, key, generation, member.public_key);
    }
  }
  return keys;
}

/**
 * What a member's device sees to: keys for members who have none yet, and
 * a new key for a folder someone left (or that has none).
 */
export async function seePending() {
  if (!unlocked) return;
  try {
    const tasks = (await listEncryptionTasks({ throwOnError: true })).data;
    for (const task of tasks) {
      const data = await fetchKeys(task.folder_id);
      const known = await readKeys(task.folder_id, data);
      const keys = await keysToPost(data, known.keys, task.needs_new_key ? random(32) : undefined);
      if (keys.length === 0) continue;
      // Another device may have made the new key first (409): read again
      // next time.
      const done = await addFolderKeys({ path: { id: task.folder_id }, body: { keys } });
      if (done.data) await readKeys(task.folder_id, done.data);
    }
  } catch (error) {
    console.warn("Encryption keys: not seen to now", error);
  }
}
