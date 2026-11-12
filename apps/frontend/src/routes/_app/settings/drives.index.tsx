import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import {
  ArrowRightLeftIcon,
  HardDriveIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
  UsersIcon,
  XIcon,
} from "lucide-react";
import { toast } from "sonner";
import type { SharedDrive } from "organizer-sdk";
import {
  createSharedDriveMutation,
  deleteSharedDriveMutation,
  listGlobalBackendsOptions,
  listSharedDrivesOptions,
  moveSharedDriveMutation,
  listTeamsOptions,
  updateSharedDriveMutation,
} from "organizer-sdk/react-query";
import { MoveDriveDialog, StoredOnField, globalChoices } from "@/components/backends";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { RenameSharedDriveDialog } from "@/components/drive-dialogs";
import { FormDialog } from "@/components/form-dialog";
import { LevelSettingsCard } from "@/components/levels";
import { ListRow } from "@/components/list-row";
import { StorageLimitDialog } from "@/components/name-dialogs";
import { OptionSelect } from "@/components/option-select";
import { RowMenu } from "@/components/row-menu";
import { SettingField, SettingsCard } from "@/components/settings";
import { SharedDriveAccessDialog } from "@/components/share-dialog";
import { SuperadminGate } from "@/components/step-up";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage, queryClient } from "@/lib/api";
import { DISK_ID, chosenBackend, movingText, unavailableText } from "@/lib/backends";
import { type NewMember, byName, formatMembers, managedBy, withMember } from "@/lib/drives";
import { NO_TEAM, chosenTeam, teamOptions } from "@/lib/teams";
import { formatUsage, parseLimitGb } from "@/lib/users";

// A superadmin page: a step-up first (`SuperadminGate`).
export const Route = createFileRoute("/_app/settings/drives/")({
  component: () => (
    <SuperadminGate>
      <SharedDrives />
      <LevelSettingsCard
        scope={{ level: "superadmin" }}
        prefixes={["drives.", "links.", "mirrors."]}
        title="Sharing"
        description="Who may make shared drives, the public links of every drive, and whether members may mirror shared drives to their own storage: a default teams and drives may change, locked, or a range."
      />
    </SuperadminGate>
  ),
});

type Dialog =
  | { kind: "new" }
  | { kind: "members"; shared: SharedDrive }
  | { kind: "rename"; shared: SharedDrive }
  | { kind: "limit"; shared: SharedDrive }
  | { kind: "move"; shared: SharedDrive }
  | { kind: "delete"; shared: SharedDrive };

/** The shared drives and their members, folders and their limits. */
function refresh() {
  for (const _id of ["listSharedDrives", "listDrives", "getDrive"]) {
    void queryClient.invalidateQueries({ queryKey: [{ _id }] });
  }
}

function SharedDrives() {
  const { user } = Route.useRouteContext();
  const drives = useQuery(listSharedDrivesOptions());
  const [dialog, setDialog] = useState<Dialog>();
  const close = () => setDialog(undefined);
  const remove = useMutation({
    ...deleteSharedDriveMutation(),
    onSuccess: (_, { path }) => {
      // Gone from the list at once, whatever a refetch in flight answers.
      queryClient.setQueryData<SharedDrive[]>(listSharedDrivesOptions().queryKey, (old) =>
        old?.filter((d) => d.drive.id !== path.id),
      );
      refresh();
      toast.success("Deleted");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const list = byName(drives.data ?? [], (d) => d.drive.name);
  return (
    <SettingsCard
      title="Shared drives"
      description="Drives of their own, apart from everyone's My drive, that only their members see. Who manages one adds and removes its members."
      action={
        <Button variant="outline" onClick={() => setDialog({ kind: "new" })}>
          <PlusIcon />
          New shared drive
        </Button>
      }
      error={drives.error}
    >
      {drives.data?.length === 0 ? (
        <p className="text-sm text-muted-foreground">No shared drives yet.</p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead className="w-28">Members</TableHead>
              <TableHead className="w-36">Size</TableHead>
              <TableHead className="w-10" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {list.map((shared) => (
              <TableRow key={shared.drive.id}>
                <TableCell className="max-w-0">
                  <div className="truncate">{shared.drive.name}</div>
                  <div className="truncate text-xs text-muted-foreground">
                    {[
                      movingText(shared.drive) ?? unavailableText(shared.drive),
                      shared.drive.backend.id !== DISK_ID && shared.drive.backend.name,
                      shared.drive.team_name && `Team ${shared.drive.team_name}`,
                      managedBy(shared.drive.managers),
                    ]
                      .filter(Boolean)
                      .join(" · ") || shared.folder}
                  </div>
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {formatMembers(shared.drive.members)}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {formatUsage(shared.drive.used, shared.drive.limit)}
                </TableCell>
                <TableCell>
                  <RowMenu
                    label="Actions"
                    actions={[
                      {
                        label: "Members…",
                        icon: <UsersIcon />,
                        onClick: () => setDialog({ kind: "members", shared }),
                      },
                      {
                        label: "Rename",
                        icon: <PencilIcon />,
                        onClick: () => setDialog({ kind: "rename", shared }),
                      },
                      {
                        label: "Storage limit…",
                        icon: <HardDriveIcon />,
                        onClick: () => setDialog({ kind: "limit", shared }),
                      },
                      {
                        label: "Move to other storage…",
                        icon: <ArrowRightLeftIcon />,
                        disabled: !!shared.drive.moving_to,
                        onClick: () => setDialog({ kind: "move", shared }),
                      },
                      {
                        label: "Delete…",
                        icon: <Trash2Icon />,
                        destructive: true,
                        onClick: () => setDialog({ kind: "delete", shared }),
                      },
                    ]}
                  />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {dialog?.kind === "new" && <NewDriveDialog onClose={close} />}
      {dialog?.kind === "members" && (
        <SharedDriveAccessDialog
          id={dialog.shared.drive.id}
          name={dialog.shared.drive.name}
          me={user.id}
          onClose={close}
        />
      )}
      {dialog?.kind === "rename" && (
        <RenameSharedDriveDialog drive={dialog.shared.drive} onClose={close} />
      )}
      {dialog?.kind === "limit" && <LimitDialog shared={dialog.shared} onClose={close} />}
      {dialog?.kind === "move" && <MoveDialog shared={dialog.shared} onClose={close} />}
      {dialog?.kind === "delete" && (
        <ConfirmDialog
          title={`Delete ${dialog.shared.drive.name}?`}
          description={
            dialog.shared.default_folder
              ? "Everything in it is deleted for good, with its trash and versions, for all its members. This can't be undone."
              : `Everything in it is deleted for good from Organizer, with its trash and versions, for all its members. The files of its folder (${dialog.shared.folder}) stay on the server. This can't be undone.`
          }
          action="Delete"
          confirmText={dialog.shared.drive.name}
          onConfirm={() => remove.mutate({ path: { id: dialog.shared.drive.id } })}
          onClose={close}
        />
      )}
    </SettingsCard>
  );
}

/**
 * "New shared drive": its name, an existing folder if wanted, a limit, its
 * team (whose managers manage it and whose members edit it), its members.
 */
function NewDriveDialog({ onClose }: { onClose: () => void }) {
  const teams = useQuery(listTeamsOptions());
  const backends = useQuery(listGlobalBackendsOptions());
  const [storedOn, setStoredOn] = useState(DISK_ID);
  const [team, setTeam] = useState(NO_TEAM);
  const [name, setName] = useState("");
  const [folder, setFolder] = useState("");
  const [limit, setLimit] = useState("");
  const [members, setMembers] = useState<NewMember[]>([]);
  const [email, setEmail] = useState("");
  const [manages, setManages] = useState(false);
  const [invalid, setInvalid] = useState(false);
  const create = useMutation({
    ...createSharedDriveMutation(),
    onSuccess: (made) => {
      refresh();
      toast.success(`${made.drive.name} is made`);
      onClose();
    },
  });
  const addMember = () => {
    setMembers(withMember(members, email, manages));
    setEmail("");
    setManages(false);
  };
  return (
    <FormDialog
      title="New shared drive"
      description="If neither its team nor anyone you add manages it, you do, as a member."
      action="Create drive"
      pending={create.isPending}
      error={invalid ? "Expected a number of GB, or nothing for no limit" : create.error}
      onSubmit={() => {
        const storage = parseLimitGb(limit);
        setInvalid(storage === undefined);
        if (storage === undefined) return;
        // An email typed but not added yet counts too.
        const all = withMember(members, email, manages);
        create.mutate({
          body: {
            name: name.trim(),
            members: all,
            ...(chosenBackend(storedOn) ? { backend_id: chosenBackend(storedOn) } : {}),
            ...(folder.trim() && !chosenBackend(storedOn) ? { folder: folder.trim() } : {}),
            ...(chosenTeam(team) ? { team_id: chosenTeam(team) } : {}),
            ...(storage !== null ? { storage_limit: storage } : {}),
          },
        });
      }}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="drive-name">Name</FieldLabel>
        <Input
          id="drive-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
          required
        />
      </Field>
      <StoredOnField
        choices={globalChoices(backends.data)}
        value={storedOn}
        onChange={setStoredOn}
      />
      {!chosenBackend(storedOn) && (
        <SettingField
          id="drive-folder"
          label="Folder on the server (optional)"
          description="An existing folder to make it of; Organizer indexes it where it is. Empty for a new folder in the data folder."
        >
          <Input
            id="drive-folder"
            placeholder="/mnt/nas/team"
            value={folder}
            onChange={(e) => setFolder(e.target.value)}
          />
        </SettingField>
      )}
      <SettingField
        id="drive-limit"
        label="Storage limit in GB"
        description="Empty for no limit, or the storage's limit for new drives."
      >
        <Input
          id="drive-limit"
          inputMode="decimal"
          placeholder="No limit"
          value={limit}
          onChange={(e) => setLimit(e.target.value)}
        />
      </SettingField>
      <SettingField
        id="drive-team"
        label="Team"
        description="Its managers manage the drive, its members edit it, and its storage counts toward the team's limit."
      >
        <OptionSelect
          options={teamOptions(teams.data)}
          value={team}
          onChange={setTeam}
          label="Team"
          className="w-full"
        />
      </SettingField>
      <Field>
        <FieldLabel htmlFor="drive-member">Members</FieldLabel>
        <div className="flex gap-2">
          <Input
            id="drive-member"
            type="email"
            placeholder="Email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            onKeyDown={(e) => {
              // Enter adds the member rather than making the drive.
              if (e.key === "Enter") {
                e.preventDefault();
                addMember();
              }
            }}
          />
          <Button type="button" variant="outline" disabled={!email.trim()} onClick={addMember}>
            Add
          </Button>
        </div>
      </Field>
      <Field orientation="horizontal">
        <Switch id="drive-member-manages" checked={manages} onCheckedChange={setManages} />
        <FieldLabel htmlFor="drive-member-manages">Manages</FieldLabel>
      </Field>
      {members.length > 0 && (
        <div className="divide-y">
          {members.map((member) => (
            <ListRow key={member.email} title={member.email}>
              <Switch
                aria-label={`${member.email} manages`}
                checked={member.manages}
                onCheckedChange={(on) => setMembers(withMember(members, member.email, on))}
              />
              <span className="w-16 text-xs text-muted-foreground">Manages</span>
              <Button
                type="button"
                variant="ghost"
                size="icon-sm"
                aria-label={`Remove ${member.email}`}
                onClick={() => setMembers(members.filter((m) => m !== member))}
              >
                <XIcon />
              </Button>
            </ListRow>
          ))}
        </div>
      )}
    </FormDialog>
  );
}

/** "Storage limit…" of a shared drive: none, or so many GB. */
function LimitDialog({ shared, onClose }: { shared: SharedDrive; onClose: () => void }) {
  const save = useMutation({
    ...updateSharedDriveMutation(),
    onSuccess: () => {
      refresh();
      toast.success("Storage limit saved");
      onClose();
    },
  });
  return (
    <StorageLimitDialog
      description={`${shared.drive.name} uses ${formatUsage(shared.drive.used, null)}: files, trash and versions. Writes past the limit are refused.`}
      limit={shared.drive.limit}
      pending={save.isPending}
      error={save.error}
      onSave={(limit) =>
        save.mutate({ path: { id: shared.drive.id }, body: { storage_limit: limit } })
      }
      onClose={onClose}
    />
  );
}

/** "Move to other storage…" of a shared drive: the server's disk or any global backend. */
function MoveDialog({ shared, onClose }: { shared: SharedDrive; onClose: () => void }) {
  const backends = useQuery(listGlobalBackendsOptions());
  const move = useMutation({
    ...moveSharedDriveMutation(),
    onSuccess: (moved) => {
      refresh();
      toast.success(movingText(moved.drive) ?? "Moving");
      onClose();
    },
  });
  return (
    <MoveDriveDialog
      drive={shared.drive}
      choices={globalChoices(backends.data)}
      pending={move.isPending}
      error={move.error}
      onMove={(backend_id) => move.mutate({ path: { id: shared.drive.id }, body: { backend_id } })}
      onClose={onClose}
    />
  );
}
