import { Fragment, useEffect, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  ArrowRightLeftIcon,
  CloudOffIcon,
  CopyIcon,
  FolderIcon,
  FolderInputIcon,
  FolderOpenIcon,
  GlobeIcon,
  HardDriveIcon,
  LockIcon,
  LockOpenIcon,
  MoreHorizontalIcon,
  Trash2Icon,
  UsersIcon,
} from "lucide-react";
import {
  acceptSharedMutation,
  declineSharedMutation,
  listDrivesOptions,
  listDrivesQueryKey,
} from "organizer-sdk/react-query";
import type { Drive } from "organizer-sdk/types";
import { toast } from "sonner";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { SettingsCard, SwitchField } from "@/components/settings";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Item,
  ItemActions,
  ItemContent,
  ItemDescription,
  ItemGroup,
  ItemMedia,
  ItemSeparator,
  ItemTitle,
} from "@/components/ui/item";
import { errorMessage } from "@/lib/api";
import { IgnoreDialog, LocalPlaceDialog, SyncDriveDialog } from "../components/folder-dialogs";
import { Page } from "../components/page";
import { type Encryption, type SyncedFolder, type View, commands } from "../lib/desktop";
import {
  type DriveRow,
  driveLabel,
  driveRows,
  driveSummary,
  folderRole,
  mirrorState,
  remoteOrigin,
  remoteState,
  roleLabel,
  roleNote,
  storageKind,
  storageNotes,
  storageState,
  syncable,
} from "../lib/drives";
import { modeLabel } from "../lib/status";

type Dialog =
  | { kind: "sync"; drive: Drive }
  | { kind: "place"; folder: SyncedFolder }
  | { kind: "remove"; folder: SyncedFolder }
  | { kind: "ignore" }
  | { kind: "sign-out" };

const failed = (error: unknown) => toast.error(errorMessage(error));

/** The account, encryption, the synced folders, the drop box folder, start at login, the ignored files. */
export function SettingsPage({ view }: { view: View }) {
  const [dialog, setDialog] = useState<Dialog>();
  const close = () => setDialog(undefined);
  const signOut = useMutation({ mutationFn: commands.signOut, onError: failed });
  const remove = useMutation({ mutationFn: commands.removeFolder, onError: failed });
  const dropBox = useMutation({ mutationFn: commands.setDropBox, onError: failed });
  const autostart = useMutation({ mutationFn: commands.setAutostart, onError: failed });
  const drives = useQuery(listDrivesOptions());
  const { rows, mirrors, remotes, others } = driveRows(drives.data ?? [], view.folders);
  // A folder that becomes unavailable, or available again: the drives
  // changed (left, joined, deleted).
  const unavailable = view.status.unavailable.join();
  // A drive's storage away or back, or a move started or done.
  const waiting = view.status.waiting.map((w) => `${w.folder}:${w.why}`).join();
  const { refetch } = drives;
  useEffect(() => void refetch(), [unavailable, waiting, refetch]);

  return (
    <Page title="Settings">
      <SettingsCard title="Account">
        <Item className="px-0">
          <ItemContent>
            <ItemTitle>{view.email}</ItemTitle>
            <ItemDescription>
              Signed in on {view.server} as {view.device_name}
            </ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button
              variant="outline"
              disabled={signOut.isPending}
              onClick={() => setDialog({ kind: "sign-out" })}
            >
              Sign out
            </Button>
          </ItemActions>
        </Item>
      </SettingsCard>

      {view.status.encryption.set_up && <EncryptionCard encryption={view.status.encryption} />}

      <SettingsCard
        title="Synced folders"
        description="Choose the drives, or folders in them, kept the same here and on the server, both ways."
      >
        <ItemGroup className="gap-0 rounded-lg border">
          {rows.map((row, i) => (
            <Fragment key={row.drive.id}>
              {i > 0 && <ItemSeparator className="my-0" />}
              <DriveRowItems row={row} onDialog={setDialog} />
            </Fragment>
          ))}
          {others.map((folder, i) => (
            <Fragment key={folder.id}>
              {(rows.length > 0 || i > 0) && <ItemSeparator className="my-0" />}
              <FolderRow folder={folder} onDialog={setDialog} />
            </Fragment>
          ))}
          {rows.length === 0 && others.length === 0 && (
            <p className="p-3 text-sm text-muted-foreground">
              {drives.isError ? errorMessage(drives.error) : "Loading your drives…"}
            </p>
          )}
        </ItemGroup>
        {mirrors.length > 0 && (
          <>
            <h3 className="mt-2 text-sm font-medium">Mirrors</h3>
            <p className="text-sm text-muted-foreground">
              Read-only copies of drives in your own storage: synced here, never sent back.
            </p>
            <ItemGroup className="gap-0 rounded-lg border">
              {mirrors.map((row, i) => (
                <Fragment key={row.drive.id}>
                  {i > 0 && <ItemSeparator className="my-0" />}
                  <DriveRowItems row={row} onDialog={setDialog} />
                </Fragment>
              ))}
            </ItemGroup>
          </>
        )}
        {remotes.length > 0 && (
          <>
            <h3 className="mt-2 text-sm font-medium">From other servers</h3>
            <p className="text-sm text-muted-foreground">
              Drives people on other servers shared with you. Their files go through your server.
            </p>
            <ItemGroup className="gap-0 rounded-lg border">
              {remotes.map((row, i) => (
                <Fragment key={row.drive.id}>
                  {i > 0 && <ItemSeparator className="my-0" />}
                  <DriveRowItems row={row} onDialog={setDialog} />
                </Fragment>
              ))}
            </ItemGroup>
          </>
        )}
      </SettingsCard>

      <SettingsCard title="Drop box folder">
        <SwitchField
          id="drop-box"
          label="Send what is put in the Drop box folder to the drop box"
          description={view.drop_box_path}
          checked={view.drop_box}
          disabled={dropBox.isPending}
          onChange={(enabled) => dropBox.mutate(enabled)}
        />
      </SettingsCard>

      <SettingsCard title="Start at login">
        <SwitchField
          id="autostart"
          label="Start Organizer when you log in"
          description="It starts in the tray, without its window."
          checked={view.autostart}
          disabled={autostart.isPending}
          onChange={(enabled) => autostart.mutate(enabled)}
        />
      </SettingsCard>

      <SettingsCard title="Ignored files" description="Never synced, on either side.">
        <div className="flex items-start justify-between gap-4">
          <div className="flex flex-wrap gap-1.5">
            {view.ignore.map((pattern) => (
              <Badge key={pattern} variant="outline" className="font-mono">
                {pattern}
              </Badge>
            ))}
          </div>
          <Button variant="outline" onClick={() => setDialog({ kind: "ignore" })}>
            Edit
          </Button>
        </div>
      </SettingsCard>

      {dialog?.kind === "sync" && (
        <SyncDriveDialog
          drive={dialog.drive}
          organizerDir={view.organizer_dir}
          taken={[...view.folders.map((f) => f.local_path), view.drop_box_path]}
          onDemand={view.on_demand}
          onClose={close}
        />
      )}
      {dialog?.kind === "place" && <LocalPlaceDialog folder={dialog.folder} onClose={close} />}
      {dialog?.kind === "ignore" && <IgnoreDialog patterns={view.ignore} onClose={close} />}
      {dialog?.kind === "remove" && (
        <ConfirmDialog
          title={`Stop syncing ${dialog.folder.name}?`}
          description="Its files stay on this computer and on the server; they are just no longer kept the same."
          action="Remove"
          onConfirm={() => remove.mutate(dialog.folder.id)}
          onClose={close}
        />
      )}
      {dialog?.kind === "sign-out" && (
        <ConfirmDialog
          title="Sign out?"
          description="Syncing stops, and this computer's sign-in is revoked on the server. The synced files stay where they are."
          action="Sign out"
          onConfirm={() => signOut.mutate()}
          onClose={close}
        />
      )}
    </Page>
  );
}

/**
 * A drive (a mirror too, docs/M7D_DESIGN.md §5: labelled as one, with its
 * state; one from another server with whose it is, where, and its state,
 * docs/M7E_DESIGN.md §5) and its synced folders: the whole drive's
 * actions, or Sync… (Accept or Decline first for one from another server).
 */
function DriveRowItems({ row, onDialog }: { row: DriveRow; onDialog: (dialog: Dialog) => void }) {
  const { drive } = row;
  const mirror = mirrorState(drive);
  const origin = remoteOrigin(drive);
  const remote = remoteState(drive);
  const queryClient = useQueryClient();
  const answered = {
    onSuccess: () => queryClient.invalidateQueries({ queryKey: listDrivesQueryKey() }),
    onError: failed,
  };
  const accept = useMutation({ ...acceptSharedMutation(), ...answered });
  const decline = useMutation({ ...declineSharedMutation(), ...answered });
  const answer = (mutation: typeof accept) =>
    drive.root_id && mutation.mutate({ path: { id: drive.root_id } });
  return (
    <>
      <Item size="sm">
        <ItemMedia variant="icon" className="text-muted-foreground">
          {drive.kind === "my" ? (
            <HardDriveIcon />
          ) : drive.kind === "mirror" ? (
            <CopyIcon />
          ) : drive.kind === "remote" ? (
            <GlobeIcon />
          ) : (
            <UsersIcon />
          )}
        </ItemMedia>
        <ItemContent className="min-w-0">
          <ItemTitle>
            {driveLabel(drive)}
            {drive.my_role && <Badge variant="outline">{roleLabel(drive.my_role)}</Badge>}
          </ItemTitle>
          <ItemDescription className="truncate">{driveSummary(row)}</ItemDescription>
          {origin && <ItemDescription className="truncate">{origin}</ItemDescription>}
          {mirror && <ItemDescription className="text-foreground">{mirror}</ItemDescription>}
          {remote && <ItemDescription className="text-foreground">{remote}</ItemDescription>}
          {roleNote(drive.my_role) && <ItemDescription>{roleNote(drive.my_role)}</ItemDescription>}
          <StorageLines drive={drive} />
        </ItemContent>
        <ItemActions>
          {row.whole ? (
            <FolderActions folder={row.whole} onDialog={onDialog} />
          ) : drive.remote?.state === "pending" ? (
            <>
              <Button
                variant="outline"
                size="sm"
                disabled={decline.isPending || accept.isPending}
                onClick={() => answer(decline)}
              >
                Decline
              </Button>
              <Button
                size="sm"
                disabled={decline.isPending || accept.isPending}
                onClick={() => answer(accept)}
              >
                Accept
              </Button>
            </>
          ) : (
            <Button
              variant="outline"
              size="sm"
              disabled={!syncable(drive)}
              onClick={() => onDialog({ kind: "sync", drive })}
            >
              Sync…
            </Button>
          )}
        </ItemActions>
      </Item>
      {row.folders.map((folder) => (
        <FolderRow key={folder.id} folder={folder} drive={drive} nested onDialog={onDialog} />
      ))}
    </>
  );
}

/**
 * A drive's storage, where its members should know (docs/M7C_DESIGN.md):
 * on someone's own storage, not backed up, and why it does not sync now
 * (unavailable, or being moved).
 */
function StorageLines({ drive }: { drive: Drive }) {
  const state = storageState(drive);
  const kind = storageKind(drive);
  const icon =
    kind === "locked" ? (
      <LockIcon />
    ) : kind === "moving" ? (
      <ArrowRightLeftIcon />
    ) : (
      <CloudOffIcon />
    );
  return (
    <>
      {storageNotes(drive).map((note) => (
        <ItemDescription key={note}>{note}</ItemDescription>
      ))}
      {state && (
        <ItemDescription className="flex items-center gap-1 text-foreground [&>svg]:size-3.5 [&>svg]:shrink-0">
          {icon}
          {state}
        </ItemDescription>
      )}
    </>
  );
}

/** A synced folder: in a drive (`nested` under it), or of a drive no longer listed. */
function FolderRow({
  folder,
  drive = null,
  nested = false,
  onDialog,
}: {
  folder: SyncedFolder;
  drive?: Drive | null;
  nested?: boolean;
  onDialog: (dialog: Dialog) => void;
}) {
  // Its own role, where it is not its drive's.
  const role = folderRole(folder, drive);
  return (
    <Item size="sm" className={nested ? "pl-10" : undefined}>
      <ItemMedia variant="icon" className="text-muted-foreground">
        <FolderIcon />
      </ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>
          {folder.name}
          {role && <Badge variant="outline">{roleLabel(role)}</Badge>}
        </ItemTitle>
        <ItemDescription className="truncate">{folder.local_path}</ItemDescription>
        {role && roleNote(role) && <ItemDescription>{roleNote(role)}</ItemDescription>}
      </ItemContent>
      <ItemActions>
        <FolderActions folder={folder} onDialog={onDialog} />
      </ItemActions>
    </Item>
  );
}

/** How a synced folder is kept (or that it is unavailable), and what can be done with it. */
function FolderActions({
  folder,
  onDialog,
}: {
  folder: SyncedFolder;
  onDialog: (dialog: Dialog) => void;
}) {
  return (
    <>
      {folder.unavailable ? (
        <Badge
          variant="outline"
          title="No longer on the server, or you left its drive: not synced. Its files stay here."
        >
          Unavailable
        </Badge>
      ) : folder.waiting ? (
        <Badge variant="outline" title={folder.waiting.reason}>
          {folder.waiting.why === "moving" ? "Read-only" : "Waiting"}
        </Badge>
      ) : (
        <Badge variant="secondary">{modeLabel(folder.mode)}</Badge>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger
          render={<Button variant="ghost" size="icon-sm" aria-label="More actions" />}
        >
          <MoreHorizontalIcon />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end">
          <DropdownMenuItem onClick={() => void commands.openPath(folder.local_path).catch(failed)}>
            <FolderOpenIcon />
            Open
          </DropdownMenuItem>
          {folder.mode === "sync" && (
            <DropdownMenuItem onClick={() => onDialog({ kind: "place", folder })}>
              <FolderInputIcon />
              Change local place…
            </DropdownMenuItem>
          )}
          <DropdownMenuSeparator />
          <DropdownMenuItem
            variant="destructive"
            onClick={() => onDialog({ kind: "remove", folder })}
          >
            <Trash2Icon />
            {folder.unavailable ? "Remove" : "Stop syncing"}
          </DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  );
}

/**
 * The end-to-end-encrypted folders on this computer: locked, the
 * encryption passphrase unlocks them (the key goes to the system's
 * credential store); unlocked, Lock forgets it here.
 */
function EncryptionCard({ encryption }: { encryption: Encryption }) {
  const [passphrase, setPassphrase] = useState("");
  const unlock = useMutation({
    mutationFn: () => commands.unlockEncryption(passphrase),
    onSuccess: () => setPassphrase(""),
  });
  const lock = useMutation({ mutationFn: commands.lockEncryption, onError: failed });
  return (
    <SettingsCard
      title="Encryption"
      description="Your encrypted folders are synced decrypted on this computer once it is unlocked."
    >
      {encryption.unlocked ? (
        <Item className="px-0">
          <ItemMedia variant="icon" className="text-muted-foreground">
            <LockOpenIcon />
          </ItemMedia>
          <ItemContent>
            <ItemTitle>Unlocked on this device</ItemTitle>
            <ItemDescription>Its key is kept in this computer's credential store.</ItemDescription>
          </ItemContent>
          <ItemActions>
            <Button variant="outline" disabled={lock.isPending} onClick={() => lock.mutate()}>
              Lock
            </Button>
          </ItemActions>
        </Item>
      ) : (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            unlock.mutate();
          }}
        >
          <Field data-invalid={unlock.isError || undefined}>
            <FieldLabel htmlFor="encryption-passphrase">Encryption passphrase</FieldLabel>
            <div className="flex gap-2">
              <Input
                id="encryption-passphrase"
                type="password"
                autoComplete="current-password"
                value={passphrase}
                aria-invalid={unlock.isError || undefined}
                onChange={(event) => setPassphrase(event.target.value)}
              />
              <Button type="submit" disabled={!passphrase || unlock.isPending}>
                Unlock
              </Button>
            </div>
            {unlock.isError ? (
              <FieldError>{errorMessage(unlock.error)}</FieldError>
            ) : (
              <FieldDescription>
                The one you chose when you set up encryption, not your sign-in password.
              </FieldDescription>
            )}
          </Field>
        </form>
      )}
    </SettingsCard>
  );
}
