import { describe, expect, it } from "vitest";
import type { Recipients, SharedItem } from "organizer-sdk";
import {
  WAITING_FOR_ENCRYPTION,
  addableRecipients,
  expiryDate,
  expiryOf,
  grantDetail,
  grantee,
  initials,
  linkSummary,
  linkUrl,
  matchesRecipient,
  memberCount,
  newCount,
  parentName,
  sharedFrom,
  waitingElsewhere,
  withTypedAddress,
} from "./sharing";

const person = (id: string, email: string) => ({ id, email });

describe("addableRecipients", () => {
  const recipients: Recipients = {
    users: [person("u1", "anna@example.com"), person("u2", "bob@example.com")],
    groups: [{ id: "g1", name: "Staff", members: 3 }],
  };
  it("lists teams first, without those with a grant already", () => {
    expect(addableRecipients(recipients, ["u1"])).toEqual([
      { kind: "team", id: "g1", label: "Staff" },
      { kind: "user", id: "u2", label: "bob@example.com" },
    ]);
    expect(addableRecipients(undefined, [])).toEqual([]);
  });
  it("lists the contacts on other servers last, without those with a grant already", () => {
    const contact = (address: string, name: string) => ({
      id: address,
      address,
      name,
      email: "",
      server: address.split("@").at(-1) ?? "",
      provider: null,
      organizer: false,
      since: "2026-11-01T00:00:00Z",
    });
    const contacts = [contact("jonas@cloud.example", "Jonas"), contact("mia@lab.example", "")];
    expect(addableRecipients(recipients, ["u1", "u2", "g1", "mia@lab.example"], contacts)).toEqual([
      { kind: "remote", id: "jonas@cloud.example", label: "Jonas", server: "cloud.example" },
    ]);
  });
  it("match what is typed by name, or by the address on another server", () => {
    const bo = { kind: "remote", id: "bo@x.example@cloud.example", label: "Bo" } as const;
    expect(matchesRecipient(bo, "bo")).toBe(true);
    expect(matchesRecipient(bo, "BO@X.EXAMPLE@cloud")).toBe(true);
    expect(matchesRecipient(bo, "anna")).toBe(false);
    expect(matchesRecipient({ kind: "user", id: "u1", label: "anna@example.com" }, "u1")).toBe(
      false,
    );
    expect(matchesRecipient(bo, " ")).toBe(true);
  });
  it("adds an address typed that none of them is", () => {
    const list = addableRecipients(recipients, []);
    expect(withTypedAddress(list, "bob@example.com")).toEqual(list);
    expect(withTypedAddress(list, "anna")).toEqual(list);
    expect(withTypedAddress(list, "Sam@Cloud.Example").at(-1)).toEqual({
      kind: "remote",
      id: "Sam@cloud.example",
      label: "Sam@cloud.example",
      server: "cloud.example",
    });
  });
});

describe("grants", () => {
  it("are a user's or a team's", () => {
    expect(grantee({ user: person("u1", "anna@example.com"), team: null })).toEqual({
      kind: "user",
      id: "u1",
      label: "anna@example.com",
    });
    expect(grantee({ user: null, team: { id: "t", name: "Staff", members: 2 } })).toEqual({
      kind: "team",
      id: "t",
      label: "Staff",
    });
    expect(
      grantee({
        user: null,
        team: null,
        remote: { address: "j@cloud.example", name: "", server: "cloud.example", state: "pending" },
      }),
    ).toEqual({
      kind: "remote",
      id: "j@cloud.example",
      label: "j@cloud.example",
      server: "cloud.example",
    });
    expect(memberCount(1)).toBe("1 member");
    expect(memberCount(3)).toBe("3 members");
  });
  it("stop at the parent with limited access", () => {
    expect(parentName("/Documents/Taxes", "My drive")).toBe("Documents");
    expect(parentName("/Documents", "My drive")).toBe("My drive");
    expect(parentName("/", "Team")).toBe("Team");
  });
});

describe("members of an encrypted folder without a key", () => {
  const remote = (state: "pending" | "accepted" | "declined") => ({
    id: "share-1",
    user: null,
    team: null,
    created_by: null,
    remote: { address: "bob@b.example", name: "", server: "b.example", state },
  });
  it("are told on their grant's row, on another server by the share's id once accepted", () => {
    expect(grantDetail(remote("accepted"), ["share-1"], "me", "me")).toBe(WAITING_FOR_ENCRYPTION);
    expect(grantDetail(remote("accepted"), [], "me", "me")).toBe("On another server");
    expect(grantDetail(remote("pending"), ["share-1"], "me", "me")).toMatch(/^Pending/);
    const user = { id: "g", user: person("u1", "a@x"), team: null, created_by: null, remote: null };
    expect(grantDetail(user, ["u1"], "me", "me")).toBe(WAITING_FOR_ENCRYPTION);
  });
  it("are listed apart when no row shows them, by their address on another server", () => {
    const members = [
      { user_id: "u1", email: "a@x", public_key: null, remote: null },
      { user_id: "share-1", email: "bob@b.example", public_key: null, remote: "bob@b.example" },
      { user_id: "u2", email: "c@x", public_key: null, remote: null },
      { user_id: "share-2", email: "eve@c.example", public_key: null, remote: "eve@c.example" },
      { user_id: "u3", email: "d@x", public_key: "key", remote: null },
    ];
    const grants = [
      { id: "g1", user: person("u1", "a@x") },
      { id: "share-1", user: null },
    ];
    expect(waitingElsewhere(members, grants)).toEqual(["c@x", "eve@c.example"]);
  });
});

describe("initials", () => {
  it("takes the first letters of the name's parts", () => {
    expect(initials("anna.berg@example.com")).toBe("AB");
    expect(initials("bob@example.com")).toBe("BO");
    expect(initials("x@example.com")).toBe("X");
    expect(initials("Jonas Nilsen")).toBe("JN");
  });
});

describe("newCount and sharedFrom", () => {
  const owner = person("o", "owner@example.com");
  const item = (extra: Partial<SharedItem>): SharedItem =>
    ({ new: false, owner, shared_by: owner, group: null, ...extra }) as SharedItem;
  it("counts the new ones", () => {
    expect(newCount([item({ new: true }), item({}), item({ new: true })])).toBe(2);
    expect(newCount(undefined)).toBe(0);
  });
  it("says who shared it, and through which group", () => {
    expect(sharedFrom(item({}))).toBe("owner@example.com");
    expect(sharedFrom(item({ group: "Staff" }))).toBe("owner@example.com, to Staff");
    expect(sharedFrom(item({ shared_by: person("b", "bob@example.com") }))).toBe(
      "bob@example.com (owner@example.com's)",
    );
    const remote = {
      server: "cloud.example",
      owner: "alice",
      owner_name: "Alice",
      shared_by: "alice",
      state: "pending" as const,
      error: null,
    };
    expect(sharedFrom(item({ remote }))).toBe("alice@cloud.example");
  });
});

describe("links", () => {
  it("summarise their kind, password and expiry", () => {
    const now = new Date("2026-11-08T12:00:00Z");
    const link = { kind: "view" as const, has_password: false, expires_at: null };
    expect(linkSummary(link, now)).toBe("View");
    expect(
      linkSummary({ ...link, has_password: true, expires_at: "2026-12-01T22:59:59Z" }, now),
    ).toBe("View · password · expires Dec 1");
    expect(
      linkSummary({ kind: "drop", has_password: false, expires_at: "2027-01-05T12:00:00Z" }, now),
    ).toBe("File drop · expires Jan 5, 2027");
    expect(linkSummary({ ...link, expires_at: "2026-11-01T00:00:00Z" }, now)).toBe(
      "View · expired",
    );
  });
  it("have their page on this server", () => {
    expect(linkUrl("abc", "https://files.example.com")).toBe("https://files.example.com/s/abc");
  });
  it("expire at the end of the day picked", () => {
    expect(expiryOf("")).toBeNull();
    expect(expiryOf("2026-12-01")).toBe("2026-12-01T23:59:59.000Z");
    expect(expiryDate(expiryOf("2026-12-01"))).toBe("2026-12-01");
    expect(expiryDate(null)).toBe("");
  });
});
