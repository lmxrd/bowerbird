import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import {
  BanIcon,
  CircleCheckIcon,
  FolderIcon,
  HardDriveIcon,
  MoreHorizontalIcon,
  ShieldIcon,
  ShieldOffIcon,
  Trash2Icon,
  UserPlusIcon,
} from "lucide-react";
import { toast } from "sonner";
import type { Invite, UpdateUser, UserEntry } from "organizer-sdk";
import {
  getJoiningOptions,
  listInvitesOptions,
  listUsersOptions,
  removeUserMutation,
  revokeInviteMutation,
  updateJoiningMutation,
  updateUserMutation,
} from "organizer-sdk/react-query";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { CopyButton } from "@/components/copy-button";
import { FormDialog } from "@/components/form-dialog";
import { InviteDialog } from "@/components/invite-dialog";
import { ListRow } from "@/components/list-row";
import { SettingsCard, SettingField, SwitchField } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { FieldGroup } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage, queryClient } from "@/lib/api";
import { formatDate } from "@/lib/format";
import { formatExpires, formatUsage, inviteLink, limitToGb, parseLimitGb } from "@/lib/users";

// A superadmin page: a step-up first (`SuperadminGate`).
export const Route = createFileRoute("/_app/settings/users")({
  component: () => (
    <SuperadminGate>
      <Users />
    </SuperadminGate>
  ),
});

function Users() {
  return (
    <>
      <Joining />
      <UserList />
      <PendingInvites />
    </>
  );
}

/** The two switches of how people join besides an invite. */
function Joining() {
  const { session } = Route.useRouteContext();
  const joining = useQuery(getJoiningOptions());
  const save = useMutation({
    ...updateJoiningMutation(),
    onSuccess: (data) => {
      queryClient.setQueryData(getJoiningOptions().queryKey, data);
      // The login page's "Create an account" follows.
      void queryClient.invalidateQueries({ queryKey: [{ _id: "getSession" }] });
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const locked = (field: string) => joining.data?.locked.includes(field);
  return (
    <SettingsCard
      title="Joining"
      description="Besides invite links, how people can get an account here."
      error={joining.error}
    >
      {joining.data && (
        <FieldGroup>
          <SwitchField
            id="open-signup"
            label="Open sign-up"
            description={
              session.password_login
                ? "Anyone who can reach this server can create an account from the login page."
                : "Anyone your single sign-on provider signs in gets an account (passwords are off)."
            }
            lockedBy={locked("open_signup") ? "SIGNUP_OPEN" : undefined}
            checked={joining.data.open_signup}
            disabled={save.isPending}
            onChange={(open_signup) => save.mutate({ body: { open_signup } })}
          />
          <SwitchField
            id="oidc-create-users"
            label="Single sign-on creates accounts"
            description="Someone your provider signs in, with a verified email no account has, gets an account at their first sign-in."
            lockedBy={locked("oidc_create_users") ? "OIDC_CREATE_USERS" : undefined}
            checked={joining.data.oidc_create_users}
            disabled={save.isPending}
            onChange={(oidc_create_users) => save.mutate({ body: { oidc_create_users } })}
          />
        </FieldGroup>
      )}
    </SettingsCard>
  );
}

type Dialog =
  | { kind: "invite" }
  | { kind: "limit"; user: UserEntry }
  | { kind: "folder"; user: UserEntry }
  | { kind: "remove"; user: UserEntry };

function UserList() {
  const { user: me } = Route.useRouteContext();
  const users = useQuery(listUsersOptions());
  const [dialog, setDialog] = useState<Dialog>();
  const refresh = () => queryClient.invalidateQueries({ queryKey: [{ _id: "listUsers" }] });
  const update = useMutation({
    ...updateUserMutation(),
    onSuccess: () => void refresh(),
    onError: (error) => toast.error(errorMessage(error)),
  });
  const remove = useMutation({
    ...removeUserMutation(),
    onSuccess: (_, { path }) => {
      // Gone from the list at once, whatever a refetch in flight answers.
      queryClient.setQueriesData<UserEntry[]>({ queryKey: [{ _id: "listUsers" }] }, (old) =>
        old?.filter((u) => u.id !== path.id),
      );
      void refresh();
      toast.success("Removed");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const change = (user: UserEntry, body: UpdateUser, done?: string) =>
    update.mutate(
      { path: { id: user.id }, body },
      { onSuccess: () => done && toast.success(done) },
    );
  return (
    <SettingsCard
      title="Users"
      description="Everyone with an account here."
      action={
        <Button variant="outline" onClick={() => setDialog({ kind: "invite" })}>
          <UserPlusIcon />
          Invite someone
        </Button>
      }
      error={users.error}
    >
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Email</TableHead>
            <TableHead className="w-36">Storage</TableHead>
            <TableHead className="w-28">Last active</TableHead>
            <TableHead className="w-10" />
          </TableRow>
        </TableHeader>
        <TableBody>
          {users.data?.map((user) => (
            <TableRow key={user.id}>
              <TableCell className="max-w-0">
                <div className="flex items-center gap-2">
                  <span className="truncate">{user.email}</span>
                  {user.is_superadmin && <Badge variant="secondary">Superadmin</Badge>}
                  {user.disabled_at && <Badge variant="destructive">Disabled</Badge>}
                </div>
              </TableCell>
              <TableCell className="text-muted-foreground">
                {formatUsage(user.storage.used, user.storage.limit)}
              </TableCell>
              <TableCell className="text-muted-foreground">
                {user.last_seen_at ? formatDate(user.last_seen_at) : "Never"}
              </TableCell>
              <TableCell>
                <DropdownMenu>
                  <DropdownMenuTrigger
                    render={<Button variant="ghost" size="icon-sm" aria-label="Actions" />}
                  >
                    <MoreHorizontalIcon />
                  </DropdownMenuTrigger>
                  <DropdownMenuContent align="end" className="w-44">
                    {user.is_superadmin ? (
                      <DropdownMenuItem
                        onClick={() =>
                          change(user, { is_superadmin: false }, "No longer a superadmin")
                        }
                      >
                        <ShieldOffIcon />
                        Remove superadmin
                      </DropdownMenuItem>
                    ) : (
                      <DropdownMenuItem
                        onClick={() => change(user, { is_superadmin: true }, "Made a superadmin")}
                      >
                        <ShieldIcon />
                        Make superadmin
                      </DropdownMenuItem>
                    )}
                    <DropdownMenuItem onClick={() => setDialog({ kind: "limit", user })}>
                      <HardDriveIcon />
                      Storage limit…
                    </DropdownMenuItem>
                    <DropdownMenuItem onClick={() => setDialog({ kind: "folder", user })}>
                      <FolderIcon />
                      Files folder…
                    </DropdownMenuItem>
                    <DropdownMenuSeparator />
                    {user.disabled_at ? (
                      <DropdownMenuItem
                        onClick={() => change(user, { disabled: false }, "Enabled")}
                      >
                        <CircleCheckIcon />
                        Enable
                      </DropdownMenuItem>
                    ) : (
                      <DropdownMenuItem
                        disabled={user.id === me.id}
                        onClick={() => change(user, { disabled: true }, "Disabled")}
                      >
                        <BanIcon />
                        Disable
                      </DropdownMenuItem>
                    )}
                    <DropdownMenuItem
                      variant="destructive"
                      disabled={user.id === me.id}
                      onClick={() => setDialog({ kind: "remove", user })}
                    >
                      <Trash2Icon />
                      Remove…
                    </DropdownMenuItem>
                  </DropdownMenuContent>
                </DropdownMenu>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>
      {dialog?.kind === "invite" && <InviteDialog onClose={() => setDialog(undefined)} />}
      {dialog?.kind === "limit" && (
        <LimitDialog user={dialog.user} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "folder" && (
        <FolderDialog user={dialog.user} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "remove" && (
        <ConfirmDialog
          title={`Remove ${dialog.user.email}?`}
          description={
            dialog.user.files.default
              ? "Their account, sessions and keys go, with their drop box, trash, versions and their files folder in the data folder. This can't be undone."
              : `Their account, sessions and keys go, with their drop box, trash and versions. Their files folder (${dialog.user.files.files_root}) is kept. This can't be undone.`
          }
          action="Remove"
          confirmText={dialog.user.email}
          onConfirm={() => remove.mutate({ path: { id: dialog.user.id } })}
          onClose={() => setDialog(undefined)}
        />
      )}
    </SettingsCard>
  );
}

/** "Storage limit…": none, or so many GB. */
function LimitDialog({ user, onClose }: { user: UserEntry; onClose: () => void }) {
  const [text, setText] = useState(limitToGb(user.storage.limit));
  const [invalid, setInvalid] = useState(false);
  const save = useMutation({
    ...updateUserMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: [{ _id: "listUsers" }] });
      toast.success("Storage limit saved");
      onClose();
    },
  });
  return (
    <FormDialog
      title="Storage limit"
      description={`${user.email} uses ${formatUsage(user.storage.used, null)}: files, trash, versions and the drop box. Writes past the limit are refused.`}
      action="Save"
      pending={save.isPending}
      error={invalid ? "Expected a number of GB, or nothing for no limit" : save.error}
      onSubmit={() => {
        const limit = parseLimitGb(text);
        setInvalid(limit === undefined);
        if (limit !== undefined)
          save.mutate({ path: { id: user.id }, body: { storage_limit: limit } });
      }}
      onClose={onClose}
    >
      <SettingField id="limit" label="Limit in GB" description="Empty for no limit.">
        <Input
          id="limit"
          inputMode="decimal"
          placeholder="No limit"
          value={text}
          onChange={(e) => setText(e.target.value)}
          autoFocus
        />
      </SettingField>
    </FormDialog>
  );
}

/** "Files folder…": the default in the data folder, or an existing folder. */
function FolderDialog({ user, onClose }: { user: UserEntry; onClose: () => void }) {
  const [path, setPath] = useState(user.files.default ? "" : user.files.files_root);
  const save = useMutation({
    ...updateUserMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: [{ _id: "listUsers" }] });
      toast.success("Saved. The folder is being indexed.");
      onClose();
    },
  });
  return (
    <FormDialog
      title="Files folder"
      description={`Where the files of ${user.email} are. Organizer indexes it where it is and copies nothing; changing it indexes the new folder from scratch.`}
      action="Save"
      pending={save.isPending}
      error={save.error}
      onSubmit={() =>
        save.mutate({ path: { id: user.id }, body: { files_root: path.trim() || null } })
      }
      onClose={onClose}
    >
      <SettingField
        id="files-root"
        label="Path on the server"
        description="Empty for their own folder under STORAGE_URL. Never another user's folder."
      >
        <Input
          id="files-root"
          placeholder={user.files.default ? user.files.files_root : "/mnt/nas/files"}
          value={path}
          onChange={(e) => setPath(e.target.value)}
          autoFocus
        />
      </SettingField>
    </FormDialog>
  );
}

/** The invite links not used yet: copy one again, or revoke it. */
function PendingInvites() {
  const invites = useQuery(listInvitesOptions());
  const revoke = useMutation({
    ...revokeInviteMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: [{ _id: "listInvites" }] });
      toast.success("Revoked");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  if (!invites.data?.length && !invites.error) return null;
  return (
    <SettingsCard title="Pending invites" error={invites.error}>
      <div className="divide-y">
        {invites.data?.map((invite: Invite) => (
          <ListRow
            key={invite.id}
            title={
              <span className="flex items-center gap-2">
                <span className="truncate">{invite.email ?? "Anyone with the link"}</span>
                {invite.is_superadmin && <Badge variant="secondary">Superadmin</Badge>}
              </span>
            }
            detail={formatExpires(invite.expires_at)}
          >
            <CopyButton
              text={inviteLink(invite.token)}
              label="Copy link"
              copied="Link copied"
              size="sm"
            />
            <Button
              variant="outline"
              size="sm"
              onClick={() => revoke.mutate({ path: { id: invite.id } })}
            >
              Revoke
            </Button>
          </ListRow>
        ))}
      </div>
    </SettingsCard>
  );
}
