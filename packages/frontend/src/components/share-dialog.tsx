import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import {
  FolderSyncIcon,
  GlobeIcon,
  LockIcon,
  LogOutIcon,
  UserRoundCogIcon,
  XIcon,
} from "lucide-react";
import { toast } from "sonner";
import type {
  Drive,
  DriveAccess,
  Grant,
  InheritedGrant,
  MirroredBy,
  NewGrant,
  Node,
  Person,
  Role,
} from "organizer-sdk";
import {
  addDriveGrantMutation,
  addGrantMutation,
  addSharedDriveGrantMutation,
  changeDriveGrantMutation,
  changeGrantMutation,
  changeOwnerMutation,
  changeSharedDriveGrantMutation,
  getAccessOptions,
  getDriveAccessOptions,
  getFileOptions,
  getFolderKeysOptions,
  getSharedDriveAccessOptions,
  listContactsOptions,
  listMirrorsOptions,
  listRecipientsOptions,
  lookUpAddressOptions,
  removeDriveGrantMutation,
  removeGrantMutation,
  removeSharedDriveGrantMutation,
  setLimitedAccessMutation,
} from "organizer-sdk/react-query";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { FormDialog } from "@/components/form-dialog";
import { LinksSection } from "@/components/links-section";
import { ListRow } from "@/components/list-row";
import { RoleSelect } from "@/components/option-select";
import { RecipientIcon } from "@/components/recipient-icon";
import { SwitchField } from "@/components/settings";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Combobox,
  ComboboxContent,
  ComboboxEmpty,
  ComboboxInput,
  ComboboxItem,
  ComboboxList,
} from "@/components/ui/combobox";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { errorMessage, queryClient } from "@/lib/api";
import { seePending } from "@/lib/encryption";
import { remoteOffer, remoteProblem, remoteRoles } from "@/lib/federation";
import { formatDate } from "@/lib/format";
import { scheduleLabel } from "@/lib/mirrors";
import { ROLE_LABELS, atLeast, can, givableRoles } from "@/lib/roles";
import {
  type Recipient,
  addableRecipients,
  WAITING_FOR_ENCRYPTION,
  grantDetail,
  grantee,
  matchesRecipient,
  parentName,
  waitingElsewhere,
  withTypedAddress,
} from "@/lib/sharing";

const failed = (error: unknown) => toast.error(errorMessage(error));

/** Everything that shows access, fetched again after a change of it. */
function refreshAccess() {
  for (const _id of [
    "getAccess",
    "getDriveAccess",
    "getSharedDriveAccess",
    "listChildren",
    "getFile",
    "listDrives",
    "getDrive",
    "listSharedDrives",
  ]) {
    void queryClient.invalidateQueries({ queryKey: [{ _id }] });
  }
}

/** A role shown, not changed here: the owner's, an inherited one. */
function RoleText({ children }: { children: React.ReactNode }) {
  return <span className="w-32 px-2.5 text-sm text-muted-foreground">{children}</span>;
}

/** What is shared, as far as another server must take it (docs/M7E_DESIGN.md §6). */
type Shared = { wholeDrive: boolean; encrypted: boolean };

/** The role to start with: editor when it may be given, else the first. */
function firstRole(roles: Role[]): Role {
  return roles.includes("editor") ? "editor" : roles[0];
}

/**
 * "Add people or teams": whom, with which role, and Add. An address
 * typed (`user@host`) or a contact is someone on another server: their
 * server is looked up, and only what it takes is offered.
 */
function AddGrantForm({
  recipients,
  roles,
  shared,
  pending,
  onAdd,
}: {
  recipients: Recipient[];
  /** What the user may give, the highest first. */
  roles: Role[];
  shared: Shared;
  pending: boolean;
  onAdd: (recipient: Recipient, role: Role) => Promise<unknown>;
}) {
  const [chosen, setChosen] = useState<Recipient | null>(null);
  const [typed, setTyped] = useState("");
  const [role, setRole] = useState<Role>(firstRole(roles));
  const remote = chosen?.kind === "remote" ? chosen : undefined;
  const lookup = useQuery({
    ...lookUpAddressOptions({ query: { address: remote?.id ?? "" } }),
    enabled: remote !== undefined,
  });
  // Someone on another server: viewer, editor or manager (OCM has nothing else).
  const offered = remote ? remoteRoles(roles, shared.wholeDrive) : roles;
  const given = offered.includes(role) ? role : firstRole(offered);
  const problem = remote && lookup.data ? remoteProblem(lookup.data, shared) : undefined;
  const ready = chosen !== null && (!remote || (lookup.data !== undefined && !problem));
  return (
    <div className="flex min-w-0 flex-col gap-2">
      <form
        className="flex min-w-0 gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          if (!chosen || !ready) return;
          // Their address as their server is known by (the lookup's).
          const to = remote && lookup.data ? { ...remote, id: lookup.data.address } : chosen;
          void onAdd(to, given).then(() => setChosen(null), failed);
        }}
      >
        <Combobox
          items={withTypedAddress(recipients, typed)}
          value={chosen}
          onValueChange={(value) => setChosen(value as Recipient | null)}
          onInputValueChange={setTyped}
          filter={(item: Recipient, query: string) => matchesRecipient(item, query)}
          // Enter takes the first match: an address typed is added at once.
          autoHighlight
          itemToStringLabel={(item: Recipient) => item.label}
          isItemEqualToValue={(a: Recipient, b: Recipient) => a.id === b.id}
        >
          <ComboboxInput
            className="min-w-0 flex-1"
            placeholder="Add people, teams or user@server…"
            showTrigger={false}
          />
          <ComboboxContent>
            <ComboboxEmpty>
              Nobody else to add. Someone on another server: user@server.
            </ComboboxEmpty>
            <ComboboxList>
              {(item: Recipient) => (
                <ComboboxItem key={`${item.kind}-${item.id}`} value={item}>
                  <RecipientIcon kind={item.kind} label={item.label} />
                  <span className="min-w-0 flex-1 truncate">{item.label}</span>
                  {item.server && (
                    <span className="truncate text-xs text-muted-foreground">{item.server}</span>
                  )}
                </ComboboxItem>
              )}
            </ComboboxList>
          </ComboboxContent>
        </Combobox>
        <RoleSelect roles={offered} value={given} onChange={setRole} label="Role to give" />
        <Button type="submit" disabled={!ready || pending}>
          Add
        </Button>
      </form>
      {remote && (
        <p
          className={`flex items-center gap-1.5 text-sm ${problem || lookup.isError ? "text-destructive" : "text-muted-foreground"}`}
        >
          <GlobeIcon className="size-3.5 shrink-0" />
          {lookup.isPending
            ? `Asking ${remote.server}…`
            : lookup.isError
              ? errorMessage(lookup.error)
              : (problem ?? remoteOffer(lookup.data.server, offered))}
        </p>
      )}
    </div>
  );
}

/** What the access dialog changes: an item's grants or a drive's. */
type GrantActions = {
  add: (body: NewGrant) => Promise<unknown>;
  change: (grant: Grant, role: Role) => void;
  remove: (grant: Grant) => void;
  pending: boolean;
};

/**
 * Who has access, as both dialogs list it: the add field (for who may
 * manage it), the rows above the grants (the owner, who keep access), the
 * grants with their role and remove, and the inherited ones.
 */
function AccessList({
  kind,
  shared,
  myRole,
  mayManage,
  grants,
  inherited = [],
  above,
  below,
  waiting = [],
  ownerId,
  me,
  actions,
}: {
  kind: "file" | "folder";
  /** A whole drive, an encrypted folder: what another server must take. */
  shared: Shared;
  myRole: Role;
  mayManage: boolean;
  grants: Grant[];
  inherited?: InheritedGrant[];
  /** The rows before the grants: the owner, the drive's managers. */
  above?: React.ReactNode;
  /** The rows after them: who mirror the drive. */
  below?: React.ReactNode;
  /**
   * Who have no keys of an encrypted folder yet, by user id (on another
   * server, the share's id: their grant's).
   */
  waiting?: string[];
  /** The item's owner, who is not added and whose grants need no "Added by". */
  ownerId?: string;
  me: string;
  actions: GrantActions;
}) {
  const recipients = useQuery({ ...listRecipientsOptions(), enabled: mayManage });
  const contacts = useQuery({ ...listContactsOptions(), enabled: mayManage });
  const roles = givableRoles(myRole, kind);
  const taken = [...grants.map((g) => grantee(g).id), me, ...(ownerId ? [ownerId] : [])];
  const you = (label: string, id: string) => (id === me ? `${label} (you)` : label);
  // Someone on another server has their server's badge, given here or above.
  const title = (who: Recipient) =>
    who.server ? (
      <span className="flex min-w-0 items-center gap-1.5">
        <span className="truncate">{who.label}</span>
        <Badge variant="outline">{who.server}</Badge>
      </span>
    ) : (
      you(who.label, who.id)
    );
  return (
    <>
      {mayManage && roles.length > 0 && (
        <AddGrantForm
          recipients={addableRecipients(recipients.data, taken, contacts.data)}
          roles={roles}
          shared={shared}
          pending={actions.pending}
          onAdd={(r, role) =>
            actions.add(
              r.kind === "team"
                ? { team_id: r.id, role }
                : r.kind === "remote"
                  ? { address: r.id, role }
                  : { user_id: r.id, role },
            )
          }
        />
      )}
      <div className="max-h-80 min-w-0 divide-y overflow-y-auto">
        {above}
        {grants.map((grant) => {
          const who = grantee(grant);
          const detail = grantDetail(grant, waiting, me, ownerId);
          // A manager changes what is at most their own.
          const editable = mayManage && atLeast(myRole, grant.role);
          return (
            <ListRow
              key={grant.id}
              icon={<RecipientIcon kind={who.kind} label={who.label} />}
              title={title(who)}
              detail={detail}
            >
              {mayManage ? (
                <>
                  <RoleSelect
                    roles={grant.remote ? remoteRoles(roles, shared.wholeDrive) : roles}
                    value={grant.role}
                    disabled={!editable}
                    onChange={(role) => actions.change(grant, role)}
                    label={`Role of ${who.label}`}
                  />
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label={`Remove ${who.label}`}
                    disabled={!editable}
                    onClick={() => actions.remove(grant)}
                  >
                    <XIcon />
                  </Button>
                </>
              ) : (
                // Who may not change access sees the roles only.
                <>
                  <RoleText>{ROLE_LABELS[grant.role]}</RoleText>
                  <span className="size-7" />
                </>
              )}
            </ListRow>
          );
        })}
        {inherited.map((grant) => {
          const who = grantee(grant);
          return (
            <ListRow
              key={`inherited-${grant.id}`}
              icon={<RecipientIcon kind={who.kind} label={who.label} />}
              title={title(who)}
              detail={`Inherited from ${grant.from_name}`}
            >
              <RoleText>{ROLE_LABELS[grant.role]}</RoleText>
              {/* Changed where it is given; the column of the remove buttons stays. */}
              <span className="size-7" />
            </ListRow>
          );
        })}
        {below}
      </div>
    </>
  );
}

/**
 * "Mirrored by <user>" (docs/M7D_DESIGN.md §3): each member who keeps a
 * copy of the drive in their own storage; the user's own mirror says
 * where and when it copies.
 */
function MirroredByRows({
  driveId,
  mirroredBy,
  me,
}: {
  driveId: string;
  mirroredBy: MirroredBy[];
  me: string;
}) {
  const mine = useQuery({
    ...listMirrorsOptions(),
    enabled: mirroredBy.some((m) => m.user.id === me),
  });
  return mirroredBy.map((m) => {
    const own = m.user.id === me ? mine.data?.find((x) => x.drive_id === driveId) : undefined;
    const detail = own
      ? `${own.backend.name} · ${scheduleLabel(own.schedule)}`
      : `Their own storage · since ${formatDate(m.since)}`;
    return (
      <ListRow
        key={`mirror-${m.user.id}`}
        icon={<FolderSyncIcon className="size-4 shrink-0 text-muted-foreground" />}
        title={`Mirrored by ${m.user.id === me ? `${m.user.email} (you)` : m.user.email}`}
        detail={detail}
      >
        <RoleText>Mirror</RoleText>
        <span className="size-7" />
      </ListRow>
    );
  });
}

/** A person who has a role that is not a grant here: the owner, a drive's manager. */
function PersonRow({
  person,
  me,
  role,
  detail,
}: {
  person: Person;
  me: string;
  role: string;
  detail?: string;
}) {
  return (
    <ListRow
      icon={<RecipientIcon kind="user" label={person.email} />}
      title={person.id === me ? `${person.email} (you)` : person.email}
      detail={detail}
    >
      <RoleText>{role}</RoleText>
      <span className="size-7" />
    </ListRow>
  );
}

/**
 * "Share…": who has access to a file or folder and with which role, the
 * inherited access, limited access on a folder, its public links, and
 * handing it over (its owner, in My drive).
 */
export function ShareDialog({
  node,
  me,
  onClose,
}: {
  node: Node;
  /** The user's id. */
  me: string;
  onClose: () => void;
}) {
  const path = { id: node.id };
  const access = useQuery(getAccessOptions({ path }));
  const entry = useQuery(getFileOptions({ path }));
  const [handing, setHanding] = useState(false);
  // An encrypted folder: shared with people and teams only, its key
  // wrapped for each member by this device (or theirs, once set up).
  const encrypted = node.e2e_root === node.id;
  const keys = useQuery({ ...getFolderKeysOptions({ path }), enabled: encrypted });
  const members = keys.data?.members ?? [];
  const waiting = members.filter((m) => !m.public_key);
  const waitingOthers = waitingElsewhere(members, access.data?.grants ?? []);
  const refresh = () => {
    refreshAccess();
    if (encrypted) {
      void seePending().then(() =>
        queryClient.invalidateQueries({ queryKey: [{ _id: "getFolderKeys" }] }),
      );
    }
  };
  const add = useMutation({ ...addGrantMutation(), onSuccess: refresh });
  const change = useMutation({ ...changeGrantMutation(), onSuccess: refresh, onError: failed });
  const remove = useMutation({ ...removeGrantMutation(), onSuccess: refresh, onError: failed });
  const limit = useMutation({ ...setLimitedAccessMutation(), onSuccess: refresh, onError: failed });
  const data = access.data;
  const myRole = data?.my_role ?? node.my_role ?? "viewer";
  // The drive's name, for an item at its top: the inherited grant of the drive says it.
  const driveName =
    data?.inherited.find((g) => g.from_id === null)?.from_name ??
    (node.drive_id === me ? "My drive" : "the drive");
  const parent = parentName(entry.data?.path ?? "", driveName);
  // Who keep access, but for the owner and whoever has a grant here (a
  // previous owner's manager grant): listed once.
  const keepers = (data?.keepers ?? []).filter(
    (k) => k.id !== data?.owner?.id && !data?.grants.some((g) => g.user?.id === k.id),
  );
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Share {node.name}</DialogTitle>
          <DialogDescription>
            People and teams see it in "Shared with me", and can add it to their files. Its storage,
            versions and trash stay where it is.
          </DialogDescription>
        </DialogHeader>
        {access.isError ? (
          <p className="text-sm text-destructive">{errorMessage(access.error)}</p>
        ) : (
          data && (
            <AccessList
              kind={node.kind}
              shared={{ wholeDrive: false, encrypted: node.e2e_root != null }}
              myRole={myRole}
              mayManage={data.may_manage}
              grants={data.grants}
              inherited={data.inherited}
              waiting={waiting.map((m) => m.user_id)}
              ownerId={data.owner?.id}
              me={me}
              above={
                <>
                  {data.owner && <PersonRow person={data.owner} me={me} role="Owner" />}
                  {keepers.map((k) => (
                    <PersonRow
                      key={k.id}
                      person={k}
                      me={me}
                      role="Manager"
                      detail="Always has access"
                    />
                  ))}
                </>
              }
              actions={{
                pending: add.isPending,
                add: (body) => add.mutateAsync({ path, body }),
                change: (grant, role) =>
                  change.mutate({ path: { ...path, grant_id: grant.id }, body: { role } }),
                remove: (grant) => remove.mutate({ path: { ...path, grant_id: grant.id } }),
              }}
            />
          )
        )}
        {/* Members through a team who have no keys yet. */}
        {data && waitingOthers.length > 0 && (
          <p className="text-sm text-muted-foreground">
            {WAITING_FOR_ENCRYPTION}: {waitingOthers.join(", ")}
          </p>
        )}
        {encrypted && data?.may_manage && (
          <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
            <LockIcon className="size-3.5 shrink-0" />
            Encrypted folders can only be shared with Organizer servers.
          </p>
        )}
        {node.kind === "folder" && data && (
          <SwitchField
            id="limited-access"
            label="Limited access"
            description={`Only the people listed here see this folder. Access from "${parent}" stops here.`}
            checked={data.limited}
            disabled={!data.may_manage || limit.isPending}
            onChange={(limited) => limit.mutate({ path, body: { limited } })}
          />
        )}
        {can(myRole, "link") && (
          <>
            <Separator />
            {encrypted ? (
              <p className="text-sm text-muted-foreground">
                Links aren't available for encrypted folders.
              </p>
            ) : (
              <LinksSection node={node} role={myRole} />
            )}
          </>
        )}
        <DialogFooter>
          {/* Handing over is the owner's, in My drive. */}
          {myRole === "owner" && node.drive_id === me && node.parent_id && (
            <Button variant="outline" className="sm:mr-auto" onClick={() => setHanding(true)}>
              <UserRoundCogIcon />
              Change owner…
            </Button>
          )}
          <Button onClick={onClose}>Done</Button>
        </DialogFooter>
      </DialogContent>
      {handing && (
        <ChangeOwnerDialog
          node={node}
          onDone={() => {
            setHanding(false);
            refresh();
          }}
          onClose={() => setHanding(false)}
        />
      )}
    </Dialog>
  );
}

/** "Change owner…": someone else becomes the owner, and the user its manager. */
function ChangeOwnerDialog({
  node,
  onDone,
  onClose,
}: {
  node: Node;
  onDone: () => void;
  onClose: () => void;
}) {
  const [email, setEmail] = useState("");
  const hand = useMutation({
    ...changeOwnerMutation(),
    onSuccess: () => {
      toast.success(`${email.trim()} owns ${node.name} now`);
      onDone();
    },
  });
  return (
    <FormDialog
      title={`Change the owner of ${node.name}`}
      description="They become its owner, and you its manager. It stays where it is."
      action="Change owner"
      pending={hand.isPending}
      error={hand.error}
      onSubmit={() => hand.mutate({ path: { id: node.id }, body: { email: email.trim() } })}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="new-owner">New owner</FieldLabel>
        <Input
          id="new-owner"
          type="email"
          placeholder="Email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          autoFocus
          required
        />
      </Field>
    </FormDialog>
  );
}

/**
 * "Members…" of a shared drive, as both of its dialogs show it: who has
 * access to the whole drive and with which role (the drive's grants), its
 * team and the team's managers, and `onLeave` when the user may leave it.
 */
function DriveAccessView({
  name,
  access,
  error,
  me,
  actions,
  onLeave,
  onClose,
}: {
  name: string;
  access: DriveAccess | undefined;
  error: unknown;
  me: string;
  actions: GrantActions;
  /** Give up the user's own grant: leaving the drive. */
  onLeave?: () => void;
  onClose: () => void;
}) {
  const [leaving, setLeaving] = useState(false);
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>Members of {name}</DialogTitle>
          <DialogDescription>
            Their role reaches everything in the drive, but for folders with limited access.
            Managers decide who has access.
          </DialogDescription>
        </DialogHeader>
        {error ? (
          <p className="text-sm text-destructive">{errorMessage(error)}</p>
        ) : (
          access && (
            <AccessList
              kind="folder"
              shared={{ wholeDrive: true, encrypted: false }}
              myRole={access.my_role}
              mayManage={access.may_manage}
              grants={access.grants}
              me={me}
              above={access.team_managers.map((p) => (
                <PersonRow
                  key={p.id}
                  person={p}
                  me={me}
                  role="Manager"
                  detail={access.team ? `Manages ${access.team.name}` : undefined}
                />
              ))}
              below={
                access.mirrored_by.length > 0 && (
                  <MirroredByRows
                    driveId={access.drive_id}
                    mirroredBy={access.mirrored_by}
                    me={me}
                  />
                )
              }
              actions={actions}
            />
          )
        )}
        <DialogFooter>
          {onLeave && (
            <Button variant="outline" className="sm:mr-auto" onClick={() => setLeaving(true)}>
              <LogOutIcon />
              Leave drive
            </Button>
          )}
          <Button onClick={onClose}>Done</Button>
        </DialogFooter>
      </DialogContent>
      {leaving && onLeave && (
        <ConfirmDialog
          title={`Leave ${name}?`}
          description="You no longer see it or what is in it (unless a team of yours has access). Someone who manages it can add you again."
          action="Leave"
          onConfirm={onLeave}
          onClose={() => setLeaving(false)}
        />
      )}
    </Dialog>
  );
}

/** "Members…" of a shared drive the user is in, through the drive's access endpoints. */
export function DriveAccessDialog({
  drive,
  me,
  onClose,
}: {
  drive: Drive;
  /** The user's id, to leave. */
  me: string;
  onClose: () => void;
}) {
  const navigate = useNavigate();
  const path = { id: drive.id };
  const access = useQuery(getDriveAccessOptions({ path }));
  const add = useMutation({ ...addDriveGrantMutation(), onSuccess: refreshAccess });
  const change = useMutation({
    ...changeDriveGrantMutation(),
    onSuccess: refreshAccess,
    onError: failed,
  });
  const remove = useMutation({
    ...removeDriveGrantMutation(),
    onSuccess: refreshAccess,
    onError: failed,
  });
  const mine = access.data?.grants.find((g) => g.user?.id === me);
  return (
    <DriveAccessView
      name={drive.name}
      access={access.data}
      error={access.error}
      me={me}
      actions={{
        pending: add.isPending,
        add: (body) => add.mutateAsync({ path, body }),
        change: (grant, role) =>
          change.mutate({ path: { ...path, grant_id: grant.id }, body: { role } }),
        remove: (grant) => remove.mutate({ path: { ...path, grant_id: grant.id } }),
      }}
      onLeave={
        mine &&
        (() =>
          remove.mutate(
            { path: { ...path, grant_id: mine.id } },
            {
              onSuccess: () => {
                toast.success(`You left ${drive.name}`);
                onClose();
                void navigate({ to: "/" });
              },
            },
          ))
      }
      onClose={onClose}
    />
  );
}

/**
 * "Members…" of any shared drive in Settings → Drives, through the
 * superadmin's access endpoints (the superadmin need not be a member).
 */
export function SharedDriveAccessDialog({
  id,
  name,
  me,
  onClose,
}: {
  id: string;
  name: string;
  /** The superadmin's id. */
  me: string;
  onClose: () => void;
}) {
  const path = { id };
  const access = useQuery(getSharedDriveAccessOptions({ path }));
  const add = useMutation({ ...addSharedDriveGrantMutation(), onSuccess: refreshAccess });
  const change = useMutation({
    ...changeSharedDriveGrantMutation(),
    onSuccess: refreshAccess,
    onError: failed,
  });
  const remove = useMutation({
    ...removeSharedDriveGrantMutation(),
    onSuccess: refreshAccess,
    onError: failed,
  });
  return (
    <DriveAccessView
      name={name}
      access={access.data}
      error={access.error}
      me={me}
      actions={{
        pending: add.isPending,
        add: (body) => add.mutateAsync({ path, body }),
        change: (grant, role) =>
          change.mutate({ path: { ...path, grant_id: grant.id }, body: { role } }),
        remove: (grant) => remove.mutate({ path: { ...path, grant_id: grant.id } }),
      }}
      onClose={onClose}
    />
  );
}
