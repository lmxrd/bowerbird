import type {
  AddressLookup,
  Contact,
  FederatedInvite,
  FederationSettings,
  RemoteDrive,
  RemoteParty,
  RemoteShareState,
  Role,
} from "organizer-sdk";
import { formatDay } from "@/lib/format";
import { ROLE_LABELS } from "@/lib/roles";
import { formatExpires } from "@/lib/users";

/**
 * Federation (docs/M7E_DESIGN.md): people, shares and drives on other
 * servers, over Open Cloud Mesh. Their words, and what the pages offer,
 * from what the server says (`AddressLookup`, `Grant.remote`,
 * `SharedItem.remote`, `Drive.remote`, the refusals' `code`).
 */

/** The refusals of federation's endpoints and of the lookup, by their `code`. */
const REFUSALS: Record<string, string> = {
  federation_off: "Sharing with other servers is off here. Your administrator can turn it on.",
  server_refused: "This server doesn't share with that server.",
  contact_required:
    "You can share only with your contacts on other servers: invite them first (Settings → Contacts).",
  federation_forbidden: "This drive's managers don't allow sharing it with other servers.",
  e2e_unsupported: "Encrypted folders can only be shared with Organizer servers that take them.",
  drives_unsupported: "Their server can't take a whole drive: share a folder in it instead.",
  remote_unreachable: "Their server didn't answer. Check the address, or try again later.",
  remote_refused: "Their server refused it.",
  invite_invalid: "This invite isn't valid: it is unknown or has expired. Ask for a new one.",
  invite_used: "This invite was accepted already.",
};

/** A federation refusal's words by its `code`; none for a code that is not one. */
export function refusalSentence(code: string | null | undefined): string | undefined {
  return code ? REFUSALS[code] : undefined;
}

/**
 * An OCM address split at its last `@` (the user part may be an email
 * itself, `alice@example.com@files.example.org`); none when it is not one:
 * an empty part, spaces, a path or a scheme.
 */
export function splitAddress(text: string): { user: string; server: string } | undefined {
  const trimmed = text.trim();
  const at = trimmed.lastIndexOf("@");
  if (at <= 0 || at === trimmed.length - 1) return undefined;
  const user = trimmed.slice(0, at);
  const server = trimmed.slice(at + 1);
  if (/\s/.test(trimmed) || /[/?#]/.test(server)) return undefined;
  return { user, server: server.toLowerCase() };
}

/**
 * Where an item or a drive from another server comes from:
 * "alice@cloud.example". `shared_by` is the sharer's OCM address already;
 * a bare user is put at the server.
 */
export function remoteFrom(remote: Pick<RemoteDrive, "shared_by" | "server">): string {
  const at = splitAddress(remote.shared_by);
  return at?.server === remote.server.toLowerCase()
    ? remote.shared_by
    : `${remote.shared_by}@${remote.server}`;
}

/**
 * The roles that can be given to someone on another server, of those the
 * user may give: viewer, editor and manager (OCM's read, write and share);
 * a whole drive between Organizer servers takes upload only too. Never
 * the owner.
 */
export function remoteRoles(givable: readonly Role[], wholeDrive: boolean): Role[] {
  const takes: Role[] = wholeDrive
    ? ["manager", "editor", "viewer", "upload_only"]
    : ["manager", "editor", "viewer"];
  return givable.filter((role) => takes.includes(role));
}

/** What someone on a server may be given, for people: "On cloud.example: viewer, editor or manager." */
export function remoteOffer(server: string, roles: readonly Role[]): string {
  const names = roles.map((r) => ROLE_LABELS[r].toLowerCase());
  const list =
    names.length > 1 ? `${names.slice(0, -1).join(", ")} or ${names.at(-1)}` : (names[0] ?? "");
  return `On another server, ${server}: ${list}.`;
}

/**
 * Why the user cannot share this with the address looked up, in words;
 * none when they can. What the server refuses first, then what their
 * server cannot take: a whole drive, an encrypted folder.
 */
export function remoteProblem(
  lookup: Pick<AddressLookup, "may_share" | "refusal" | "drives" | "encrypted_folders">,
  what: { wholeDrive: boolean; encrypted: boolean },
): string | undefined {
  if (!lookup.may_share) {
    return refusalSentence(lookup.refusal) ?? "You can't share with them.";
  }
  if (what.wholeDrive && !lookup.drives) return refusalSentence("drives_unsupported");
  if (what.encrypted && !lookup.encrypted_folders) return refusalSentence("e2e_unsupported");
  return undefined;
}

/** Where a share with someone on another server stands, for people. */
export const REMOTE_STATES = {
  pending: "Pending: their server hasn't accepted yet",
  accepted: "On another server",
  declined: "Declined",
} as const satisfies Record<RemoteShareState, string>;

/** The name a person on another server goes by: their name, else their address. */
export function remoteName(party: Pick<RemoteParty, "name" | "address">): string {
  return party.name.trim() || party.address;
}

/** A contact's name: theirs, else their email, else their address. */
export function contactName(contact: Pick<Contact, "name" | "email" | "address">): string {
  return contact.name.trim() || contact.email.trim() || contact.address;
}

/**
 * What a contact's server is, from its discovery: "Organizer" for one of
 * ours, else the software it names ("Nextcloud"); none when it names none.
 */
export function serverKind(contact: Pick<Contact, "provider" | "organizer">): string | undefined {
  if (contact.organizer) return "Organizer";
  return contact.provider?.trim() || undefined;
}

/** A contact's line: "jonas@cloud.example · Nextcloud · since 3 Nov". */
export function contactDetail(
  contact: Pick<Contact, "address" | "since" | "provider" | "organizer">,
  now: Date = new Date(),
): string {
  const kind = serverKind(contact);
  return [contact.address, kind, `since ${formatDay(contact.since, now)}`]
    .filter(Boolean)
    .join(" · ");
}

/** An invite's line: who accepted it and when, or that it waits and when it expires. */
export function inviteDetail(
  invite: Pick<FederatedInvite, "accepted_at" | "accepted_by" | "expires_at" | "created_at">,
  now: Date = new Date(),
): string {
  if (invite.accepted_by) {
    const when = invite.accepted_at ? ` on ${formatDay(invite.accepted_at, now)}` : "";
    return `Accepted by ${invite.accepted_by}${when}`;
  }
  if (new Date(invite.expires_at) <= now) return "Expired";
  return `Not accepted yet · ${formatExpires(invite.expires_at, now)}`;
}

/** The invites still open: not accepted and not expired. */
export function openInvites<T extends Pick<FederatedInvite, "accepted_by" | "expires_at">>(
  invites: T[] | undefined,
  now: Date = new Date(),
): T[] {
  return (invites ?? []).filter((i) => !i.accepted_by && new Date(i.expires_at) > now);
}

/**
 * The invite an OCM accept dialog link carries (`/settings/contacts?token=…
 * &providerDomain=…`), as the accept endpoint takes it; none without both.
 */
export function inviteFromLink(
  search: { token?: string; providerDomain?: string },
  origin: string,
): string | undefined {
  const token = search.token?.trim();
  const provider = search.providerDomain?.trim();
  if (!token || !provider) return undefined;
  const query = new URLSearchParams({ token, providerDomain: provider });
  return `${origin}/settings/contacts?${query.toString()}`;
}

/** The hosts typed in a list, one per line (or comma), each once, without a scheme. */
export function parseHosts(text: string): string[] {
  const out: string[] = [];
  for (const part of text.split(/[\n,]/)) {
    const host = part
      .trim()
      .replace(/^https?:\/\//i, "")
      .replace(/\/+$/, "")
      .toLowerCase();
    if (host && !out.includes(host)) out.push(host);
  }
  return out;
}

/**
 * Why federation is off though the superadmin turned it on: no
 * `PUBLIC_URL`, so no host to be reached at. None when it is in effect as
 * chosen.
 */
export function offReason(
  settings: Pick<FederationSettings, "mode" | "effective_mode" | "host">,
): string | undefined {
  if (settings.mode === "off" || settings.effective_mode !== "off") return undefined;
  if (!settings.host) {
    return "It is off for now: set PUBLIC_URL, the address other servers reach this one at, and restart.";
  }
  return "It is off for now.";
}

/**
 * The line under a drive from another server: its server, and whether it
 * waits for the user to accept it or could not be reached.
 */
export function remoteDriveLine(
  remote: Pick<RemoteDrive, "server" | "state" | "error">,
  /** The drive's storage answers (`Drive.available`): false when that server refuses. */
  available = true,
): string {
  if (remote.error || !available) return `${remote.server} · not reachable`;
  if (remote.state === "pending") return `${remote.server} · to accept`;
  return remote.server;
}
