import type { Contact, Grant, Link, LinkKind, Member, Recipients, SharedItem } from "organizer-sdk";
import { REMOTE_STATES, contactName, remoteFrom, remoteName, splitAddress } from "@/lib/federation";

/** Someone or a team the Share dialog can add. */
export type Recipient = {
  /** `remote`: someone on another server (docs/M7E_DESIGN.md §4). */
  kind: "user" | "team" | "remote";
  /** A user's or team's id; the address of someone on another server. */
  id: string;
  /** An email, a team's name, or the name of someone on another server. */
  label: string;
  /** Their server, for someone on another server. */
  server?: string;
};

/**
 * Who can still be added: the users and teams without a grant on it yet
 * (`taken`: their ids, and addresses), teams first, then the user's
 * contacts on other servers.
 */
export function addableRecipients(
  recipients: Recipients | undefined,
  taken: readonly string[],
  contacts: readonly Contact[] = [],
): Recipient[] {
  if (!recipients) return [];
  const has = new Set(taken);
  const teams: Recipient[] = recipients.groups
    .filter((g) => !has.has(g.id))
    .map((g) => ({ kind: "team", id: g.id, label: g.name }));
  const users: Recipient[] = recipients.users
    .filter((u) => !has.has(u.id))
    .map((u) => ({ kind: "user", id: u.id, label: u.email }));
  const remote: Recipient[] = contacts
    .filter((c) => !has.has(c.address))
    .map((c) => ({ kind: "remote", id: c.address, label: contactName(c), server: c.server }));
  return [...teams, ...users, ...remote];
}

/**
 * The recipients with what was typed, when it is an address (`user@host`)
 * that none of them is: someone on another server, looked up once chosen.
 */
export function withTypedAddress(recipients: Recipient[], typed: string): Recipient[] {
  const address = splitAddress(typed);
  if (!address) return recipients;
  const text = typed.trim().toLowerCase();
  const known = recipients.some(
    (r) => r.label.toLowerCase() === text || (r.kind === "remote" && r.id.toLowerCase() === text),
  );
  if (known) return recipients;
  const id = `${address.user}@${address.server}`;
  return [...recipients, { kind: "remote", id, label: id, server: address.server }];
}

/**
 * Whether a recipient matches what is typed (any case): by its label, or
 * by the address of someone on another server ("bo@x.example@cloud.example"
 * finds the contact named "bo@x.example").
 */
export function matchesRecipient(recipient: Recipient, typed: string): boolean {
  const query = typed.trim().toLowerCase();
  if (!query) return true;
  return (
    recipient.label.toLowerCase().includes(query) ||
    (recipient.kind === "remote" && recipient.id.toLowerCase().includes(query))
  );
}

/** Whose a grant is: a user's email, a team's name, or someone on another server. */
export function grantee(
  grant: Pick<Grant, "user" | "team"> & Partial<Pick<Grant, "remote">>,
): Recipient {
  if (grant.remote) {
    return {
      kind: "remote",
      id: grant.remote.address,
      label: remoteName(grant.remote),
      server: grant.remote.server,
    };
  }
  if (grant.team) return { kind: "team", id: grant.team.id, label: grant.team.name };
  return { kind: "user", id: grant.user?.id ?? "", label: grant.user?.email ?? "" };
}

/** Said of a member of an encrypted folder who has no public key yet. */
export const WAITING_FOR_ENCRYPTION = "Waiting for them to set up encryption";

/**
 * What a grant's row says under its name: for someone on another server
 * where their share stands, or once accepted, in an encrypted folder,
 * that they have no key yet (`waiting`: the ids of the members without a
 * public key; theirs is the share's, the grant's id); for a team its
 * members; who added it, when not the owner or the user.
 */
export function grantDetail(
  grant: Pick<Grant, "id" | "user" | "team" | "remote" | "created_by">,
  waiting: string[],
  me: string,
  ownerId: string | undefined,
): string | undefined {
  if (grant.remote) {
    return grant.remote.state === "accepted" && waiting.includes(grant.id)
      ? WAITING_FOR_ENCRYPTION
      : REMOTE_STATES[grant.remote.state];
  }
  if (grant.team) return memberCount(grant.team.members);
  const who = grantee(grant).id;
  if (waiting.includes(who)) return WAITING_FOR_ENCRYPTION;
  // Who re-shared it: not the owner, nor the user (they know).
  if (grant.created_by && ![who, me, ownerId].includes(grant.created_by.id)) {
    return `Added by ${grant.created_by.email}`;
  }
  return undefined;
}

/**
 * The members of an encrypted folder without a public key whom no grant
 * row shows (members through a team), by their email, or their address on
 * another server. A grant shows a member by its user, or (on another
 * server) by its id, the member's `user_id`.
 */
export function waitingElsewhere(
  members: Pick<Member, "user_id" | "email" | "public_key" | "remote">[],
  grants: Pick<Grant, "id" | "user">[],
): string[] {
  return members
    .filter((m) => !m.public_key)
    .filter((m) => !grants.some((g) => g.user?.id === m.user_id || g.id === m.user_id))
    .map((m) => m.remote ?? m.email);
}

/** A team's members for people: "1 member", "3 members". */
export function memberCount(n: number): string {
  return `${n} ${n === 1 ? "member" : "members"}`;
}

/**
 * The folder whose access limited access stops: the parent of the item at
 * `path` ("/Documents/Taxes" gives "Documents"), the drive's name for one
 * at its top.
 */
export function parentName(path: string, driveName: string): string {
  const parts = path.split("/").filter(Boolean);
  return parts.length >= 2 ? parts[parts.length - 2] : driveName;
}

/** The initials of an email for an avatar: "ab" for "anna.berg@…". */
export function initials(email: string): string {
  const name = email.split("@")[0] ?? "";
  // A name of someone on another server ("Jonas Nilsen") splits at spaces.
  const parts = name.split(/[._\s-]+/).filter(Boolean);
  const letters = parts.length >= 2 ? `${parts[0][0]}${parts[1][0]}` : name.slice(0, 2);
  return letters.toUpperCase();
}

/** A link kind for people. */
export const LINK_KINDS = {
  view: "View",
  edit: "Edit",
  drop: "File drop",
} as const satisfies Record<LinkKind, string>;

/** A public link's page, on this server. */
export function linkUrl(token: string, origin: string = window.location.origin): string {
  return `${origin}/s/${token}`;
}

/** A link's row: "View · password · expires Dec 1". */
export function linkSummary(
  link: Pick<Link, "kind" | "has_password" | "expires_at">,
  now: Date = new Date(),
): string {
  const parts: string[] = [LINK_KINDS[link.kind]];
  if (link.has_password) parts.push("password");
  if (link.expires_at) {
    const at = new Date(link.expires_at);
    parts.push(
      at <= now
        ? "expired"
        : `expires ${at.toLocaleDateString("en", {
            day: "numeric",
            month: "short",
            ...(at.getFullYear() === now.getFullYear() ? {} : { year: "numeric" }),
          })}`,
    );
  }
  return parts.join(" · ");
}

/**
 * A date picked for a link's expiry ("2026-12-01") as the time it stops:
 * the end of that day where the user is; null for none.
 */
export function expiryOf(date: string): string | null {
  if (!date) return null;
  const at = new Date(`${date}T23:59:59`);
  return Number.isNaN(at.getTime()) ? null : at.toISOString();
}

/** A link's expiry as the date field shows it: "2026-12-01"; empty for none. */
export function expiryDate(iso: string | null | undefined): string {
  if (!iso) return "";
  const at = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
}

/** How many shared things are new since "Shared with me" was last looked at. */
export function newCount(items: SharedItem[] | undefined): number {
  return items?.filter((item) => item.new).length ?? 0;
}

/** Who a share came from: "anna@example.com", "anna@… via Staff". */
export function sharedFrom(item: SharedItem): string {
  // From another server: its sharer's address ("alice@cloud.example").
  if (item.remote) return remoteFrom(item.remote);
  const by =
    item.shared_by.id === item.owner.id
      ? item.owner.email
      : `${item.shared_by.email} (${item.owner.email}'s)`;
  return item.group ? `${by}, to ${item.group}` : by;
}
