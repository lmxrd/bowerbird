import { describe, expect, it } from "vitest";
import type { Member } from "organizer-sdk";
import { errorMessage } from "@/lib/api";
import { OWNER_UNREACHABLE, keysError, keysToPost, sealingId } from "@/lib/encryption";
import {
  b64,
  decryptName,
  encryptName,
  importPrivate,
  publicKey,
  random,
  unb64,
  unwrapFolderKey,
} from "@/lib/e2e";

/** A key pair, as a member's device has it. */
async function identity() {
  const raw = random(32);
  return { privateKey: await importPrivate(raw), public: await publicKey(raw) };
}

function member(fields: Partial<Member>): Member {
  return {
    user_id: crypto.randomUUID(),
    email: "bea@example.com",
    public_key: null,
    missing: [],
    remote: null,
    ...fields,
  };
}

describe("the keys an owner's device posts", () => {
  const folder = crypto.randomUUID();

  it("wrap for a member on another server by the share's id", async () => {
    const theirs = await identity();
    const share = crypto.randomUUID();
    const key = random(32);
    const keys = await keysToPost(
      {
        folder_id: folder,
        generation: 2,
        members: [
          member({
            user_id: share,
            email: "bob@b.example",
            remote: "bob@b.example",
            public_key: b64(theirs.public),
            missing: [1, 2],
          }),
          // Not set up there yet: nothing for them.
          member({ remote: "eve@c.example", email: "eve@c.example", missing: [1] }),
        ],
      },
      new Map([[1, key]]),
    );
    expect(keys.map((k) => [k.user_id, k.generation])).toEqual([[share, 1]]);
    const opened = await unwrapFolderKey(
      unb64(keys[0].wrapped),
      folder,
      1,
      theirs.privateKey,
      theirs.public,
    );
    expect(opened).toEqual(key);
  });

  it("make a new generation for every member with a public key, on other servers too", async () => {
    const [local, theirs] = [await identity(), await identity()];
    const members = [
      member({ public_key: b64(local.public) }),
      member({ remote: "bob@b.example", public_key: b64(theirs.public) }),
      member({ remote: "eve@c.example" }),
    ];
    const fresh = random(32);
    const keys = await keysToPost({ folder_id: folder, generation: 2, members }, new Map(), fresh);
    expect(keys.map((k) => [k.user_id, k.generation])).toEqual([
      [members[0].user_id, 3],
      [members[1].user_id, 3],
    ]);
    const opened = await unwrapFolderKey(
      unb64(keys[1].wrapped),
      folder,
      3,
      theirs.privateKey,
      theirs.public,
    );
    expect(opened).toEqual(fresh);
  });
});

describe("the id names are sealed with", () => {
  const never = () => Promise.reject(new Error("not asked"));

  it("is a folder's own here, its source's in a mirror or on another server", async () => {
    expect(await sealingId({ id: "here", source_id: null, remote: null }, never)).toBe("here");
    expect(await sealingId({ id: "here", source_id: "there", remote: "b.example" }, never)).toBe(
      "there",
    );
  });

  it("is listed again for a folder on another server that has none yet", async () => {
    const asked: string[] = [];
    const id = await sealingId({ id: "made", source_id: null, remote: "b.example" }, (id) => {
      asked.push(id);
      return Promise.resolve({ source_id: "theirs" });
    });
    expect([id, asked]).toEqual(["theirs", ["made"]]);
    await expect(
      sealingId({ id: "made", source_id: null, remote: "b.example" }, () =>
        Promise.resolve({ source_id: null }),
      ),
    ).rejects.toThrow(/isn't on its owner's server yet/);
  });

  it("opens what was sealed with it, and only that", async () => {
    const key = random(32);
    const parent = await sealingId({ id: "here", source_id: "there", remote: "b.example" }, never);
    const sealed = await encryptName(key, 1, parent, "a.txt");
    expect(await decryptName(key, "there", sealed)).toBe("a.txt");
    await expect(decryptName(key, "here", sealed)).rejects.toThrow();
  });
});

describe("asking for the keys", () => {
  it("says when the owner's server is not reachable", () => {
    const unreachable = { error: "no answer", code: "remote_unreachable" };
    expect(errorMessage(keysError(unreachable))).toBe(OWNER_UNREACHABLE);
    const other = { error: "Not found" };
    expect(keysError(other)).toBe(other);
  });
});
