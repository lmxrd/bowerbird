import { describe, expect, it } from "vitest";
import {
  REMOTE_STATES,
  contactDetail,
  contactName,
  inviteDetail,
  inviteFromLink,
  offReason,
  openInvites,
  parseHosts,
  refusalSentence,
  remoteDriveLine,
  remoteFrom,
  remoteName,
  remoteOffer,
  remoteProblem,
  remoteRoles,
  serverKind,
  splitAddress,
} from "./federation";
import { errorMessage } from "./api";

const now = new Date("2026-11-10T12:00:00Z");

describe("refusals", () => {
  it("are words by their code, never the server's message", () => {
    for (const code of [
      "federation_off",
      "server_refused",
      "contact_required",
      "federation_forbidden",
      "e2e_unsupported",
      "drives_unsupported",
      "remote_unreachable",
      "remote_refused",
      "invite_invalid",
      "invite_used",
    ]) {
      expect(refusalSentence(code)).toMatch(/\.$/);
    }
    expect(refusalSentence("name_taken")).toBeUndefined();
    expect(refusalSentence(null)).toBeUndefined();
    expect(errorMessage({ error: "upstream said 418", code: "remote_refused" })).toBe(
      "Their server refused it.",
    );
    expect(errorMessage({ error: "Not found" })).toBe("Not found");
  });
});

describe("addresses", () => {
  it("split at the last @", () => {
    expect(splitAddress("jonas@cloud.example")).toEqual({ user: "jonas", server: "cloud.example" });
    expect(splitAddress(" ana@example.com@Files.Example.org ")).toEqual({
      user: "ana@example.com",
      server: "files.example.org",
    });
    expect(splitAddress("bo@127.0.0.1:8080")).toEqual({ user: "bo", server: "127.0.0.1:8080" });
  });
  it("are not anything else", () => {
    for (const bad of ["", "jonas", "@host", "jonas@", "jo nas@host", "jonas@host/path"]) {
      expect(splitAddress(bad)).toBeUndefined();
    }
  });
  it("name where a share comes from", () => {
    expect(remoteFrom({ shared_by: "alice", server: "cloud.example" })).toBe("alice@cloud.example");
    expect(
      remoteFrom({ shared_by: "ana@example.com@127.0.0.1:8123", server: "127.0.0.1:8123" }),
    ).toBe("ana@example.com@127.0.0.1:8123");
  });
});

describe("roles for someone on another server", () => {
  it("are viewer, editor and manager, of what the user may give", () => {
    expect(remoteRoles(["manager", "editor", "viewer", "upload_only"], false)).toEqual([
      "manager",
      "editor",
      "viewer",
    ]);
    expect(remoteRoles(["editor", "viewer"], false)).toEqual(["editor", "viewer"]);
  });
  it("are said in words", () => {
    expect(remoteOffer("cloud.example", ["manager", "editor", "viewer"])).toBe(
      "On another server, cloud.example: manager, editor or viewer.",
    );
    expect(remoteOffer("cloud.example", ["viewer"])).toBe(
      "On another server, cloud.example: viewer.",
    );
  });
  it("take upload only for a whole drive", () => {
    expect(remoteRoles(["manager", "editor", "viewer", "upload_only"], true)).toContain(
      "upload_only",
    );
  });
});

describe("remoteProblem", () => {
  const ok = { may_share: true, refusal: null, drives: true, encrypted_folders: true };
  const item = { wholeDrive: false, encrypted: false };
  it("says why the server refuses", () => {
    expect(remoteProblem({ ...ok, may_share: false, refusal: "contact_required" }, item)).toBe(
      refusalSentence("contact_required"),
    );
    expect(remoteProblem({ ...ok, may_share: false, refusal: null }, item)).toBeTruthy();
  });
  it("says what their server cannot take", () => {
    expect(remoteProblem(ok, item)).toBeUndefined();
    expect(remoteProblem({ ...ok, drives: false }, { ...item, wholeDrive: true })).toBe(
      refusalSentence("drives_unsupported"),
    );
    expect(remoteProblem({ ...ok, drives: false }, item)).toBeUndefined();
    expect(remoteProblem({ ...ok, encrypted_folders: false }, { ...item, encrypted: true })).toBe(
      refusalSentence("e2e_unsupported"),
    );
  });
});

describe("people on other servers", () => {
  it("go by their name, else their address", () => {
    expect(remoteName({ name: "Jonas", address: "j@h" })).toBe("Jonas");
    expect(remoteName({ name: " ", address: "j@h" })).toBe("j@h");
    expect(contactName({ name: "", email: "j@example.com", address: "j@h" })).toBe("j@example.com");
    expect(contactName({ name: "", email: "", address: "j@h" })).toBe("j@h");
    expect(REMOTE_STATES.pending).toMatch(/^Pending/);
  });
  it("are contacts since a day", () => {
    const since = "2026-11-03T10:00:00Z";
    expect(contactDetail({ address: "j@h", since, provider: null, organizer: false }, now)).toMatch(
      /^j@h · since (3 Nov|Nov 3)$/,
    );
  });
  it("say what their server is, as its discovery does", () => {
    const since = "2026-11-03T10:00:00Z";
    expect(
      contactDetail({ address: "j@h", since, provider: "Nextcloud", organizer: false }, now),
    ).toMatch(/^j@h · Nextcloud · since (3 Nov|Nov 3)$/);
    expect(serverKind({ provider: "Organizer", organizer: true })).toBe("Organizer");
    // One of ours, whatever it calls itself.
    expect(serverKind({ provider: null, organizer: true })).toBe("Organizer");
    expect(serverKind({ provider: "  ", organizer: false })).toBeUndefined();
    expect(serverKind({ provider: null, organizer: false })).toBeUndefined();
  });
});

describe("invites", () => {
  const invite = {
    created_at: "2026-11-09T12:00:00Z",
    expires_at: "2026-11-16T12:00:00Z",
    accepted_at: null,
    accepted_by: null,
  };
  it("wait, expire, or are accepted", () => {
    expect(inviteDetail(invite, now)).toBe("Not accepted yet · expires in 6 days");
    expect(inviteDetail({ ...invite, expires_at: "2026-11-01T00:00:00Z" }, now)).toBe("Expired");
    expect(
      inviteDetail(
        { ...invite, accepted_by: "mia@lab.example", accepted_at: "2026-11-10T08:00:00Z" },
        now,
      ),
    ).toMatch(/^Accepted by mia@lab\.example on /);
  });
  it("are open until accepted or expired", () => {
    const accepted = { ...invite, accepted_by: "x@y" };
    const expired = { ...invite, expires_at: "2026-11-01T00:00:00Z" };
    expect(openInvites([invite, accepted, expired], now)).toEqual([invite]);
    expect(openInvites(undefined, now)).toEqual([]);
  });
  it("come from an accept dialog's link", () => {
    expect(
      inviteFromLink({ token: "abc", providerDomain: "cloud.example" }, "https://files.example"),
    ).toBe("https://files.example/settings/contacts?token=abc&providerDomain=cloud.example");
    expect(inviteFromLink({ token: "abc" }, "https://files.example")).toBeUndefined();
    expect(inviteFromLink({}, "https://files.example")).toBeUndefined();
  });
});

describe("the superadmin's lists", () => {
  it("take a host per line, each once, without a scheme", () => {
    expect(
      parseHosts("Cloud.Example\nhttps://lab.example/\n\ncloud.example, x.example:8443"),
    ).toEqual(["cloud.example", "lab.example", "x.example:8443"]);
    expect(parseHosts("  ")).toEqual([]);
  });
  it("say why federation is off when it was turned on", () => {
    expect(offReason({ mode: "off", effective_mode: "off", host: null })).toBeUndefined();
    expect(offReason({ mode: "open", effective_mode: "open", host: "h" })).toBeUndefined();
    expect(offReason({ mode: "listed", effective_mode: "off", host: null })).toMatch(/PUBLIC_URL/);
  });
});

describe("remoteDriveLine", () => {
  it("says the server, and whether it waits or fails", () => {
    expect(remoteDriveLine({ server: "lab.example", state: "accepted", error: null })).toBe(
      "lab.example",
    );
    expect(remoteDriveLine({ server: "lab.example", state: "pending", error: null })).toBe(
      "lab.example · to accept",
    );
    expect(remoteDriveLine({ server: "lab.example", state: "accepted", error: "timeout" })).toBe(
      "lab.example · not reachable",
    );
    expect(remoteDriveLine({ server: "lab.example", state: "accepted", error: null }, false)).toBe(
      "lab.example · not reachable",
    );
  });
});
