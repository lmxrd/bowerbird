import { useEffect, useRef, useState } from "react";
import { type UseMutationOptions, useMutation, useQuery } from "@tanstack/react-query";
import { Link, useRouteContext } from "@tanstack/react-router";
import {
  ArchiveRestoreIcon,
  DownloadIcon,
  FolderSyncIcon,
  GlobeIcon,
  HardDriveUploadIcon,
  FolderOpenIcon,
  FolderPlusIcon,
  HistoryIcon,
  LoaderIcon,
  LockIcon,
  TriangleAlertIcon,
  MoreHorizontalIcon,
  PencilIcon,
  FolderInputIcon,
  Settings2Icon,
  FolderMinusIcon,
  Share2Icon,
  Trash2Icon,
  UploadIcon,
  UsersIcon,
} from "lucide-react";
import { toast } from "sonner";
import type { Drive, Entry, Node } from "organizer-sdk";
import {
  createFolderMutation,
  deleteFileMutation,
  getFileOptions,
  getMirrorOptions,
  listBackendsOptions,
  listChildrenOptions,
  listDriveSettingsOptions,
  moveFileMutation,
  unplaceSharedMutation,
} from "organizer-sdk/react-query";
import { BackupDialog } from "@/components/backup-dialog";
import { RenameDriveDialog } from "@/components/drive-dialogs";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { UnlockDialog } from "@/components/encryption-dialogs";
import { FormDialog } from "@/components/form-dialog";
import { KindIcon } from "@/components/kind-icon";
import { useMySettings } from "@/components/levels";
import { EjectDialog, MirrorDialog, MirrorStatusBadge } from "@/components/mirrors";
import { type Crumb, PageHeader } from "@/components/page-header";
import { RemoteDriveBanner } from "@/components/remote-drive";
import { DriveAccessDialog, ShareDialog } from "@/components/share-dialog";
import { VersionsDialog } from "@/components/versions-dialog";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
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
import { movingText, ownStorageWarning, storedOn } from "@/lib/backends";
import {
  childNamed,
  decryptedFile,
  openKeys,
  isUnlocked,
  keyMaterial,
  sealedName,
  seePending,
  shownName,
  useUnlocked,
} from "@/lib/encryption";
import { type DriveAt, driveRoot, entryAt } from "@/lib/drive-files";
import { MY_DRIVE, driveSummary, driveUrl } from "@/lib/drives";
import { remoteFrom } from "@/lib/federation";
import { formatDate, formatSize } from "@/lib/format";
import { jsonOf, shownValue } from "@/lib/levels";
import { lastCopyText, mayMirror, mirrorTitle, sealedAs, statusBanner } from "@/lib/mirrors";
import { ROLE_LABELS, can as allows } from "@/lib/roles";
import { plainSize } from "@/lib/e2e";
import { downloadUrl, inlineType, saveBlob, uploadEncryptedFile, uploadFile } from "@/lib/transfer";

/**
 * The crumbs of a folder's path: its drive, then each folder down to it.
 * The links are the stored names; the labels what the user sees (inside an
 * encrypted folder, decrypted).
 */
function crumbsOf(drive: DriveAt, path: string, labels?: string[]): Crumb[] {
  const parts = path.split("/").filter(Boolean);
  return [
    { label: drive.name, to: driveUrl(drive, "/") },
    ...parts.map((name, i) => ({
      label: labels?.[i] ?? name,
      to: driveUrl(drive, `/${parts.slice(0, i + 1).join("/")}`),
    })),
  ];
}

/** Whether a node is inside an encrypted folder (not its top): its name is sealed. */
function sealed(node: Pick<Node, "id" | "e2e_root">) {
  return node.e2e_root != null && node.e2e_root !== node.id;
}

/**
 * Whether a folder's encrypted folder is sealed with the ids where it comes
 * from (`source_id`): in a mirror, or shared from another server
 * (`remote`, whose folder made a moment ago has no source yet).
 */
function sealedElsewhere(folder: Pick<Node, "e2e_root" | "source_id" | "remote">) {
  return folder.e2e_root != null && (folder.source_id != null || folder.remote != null);
}

/** The names of a folder's path as the user sees them, once unlocked. */
function useShownPath(drive: DriveAt, folder: Entry, unlocked: boolean) {
  const parts = folder.path.split("/").filter(Boolean);
  const labels = useQuery({
    queryKey: ["shown-path", drive.id, folder.path, folder.id],
    enabled: folder.e2e_root != null && unlocked,
    queryFn: async () => {
      const out: string[] = [];
      // In a mirror, or shared from another server, names are sealed with
      // the ids there (`sealedAs`).
      let parent: Node | undefined = sealedElsewhere(folder) ? await driveRoot(drive) : undefined;
      let rootSource: string | null | undefined;
      for (let i = 0; i < parts.length; i++) {
        const path = `/${parts.slice(0, i + 1).join("/")}`;
        const node = await entryAt(drive, path);
        if (node.e2e_root === node.id) rootSource = node.source_id;
        out.push(await shownName(sealedAs(node, parent?.source_id, rootSource)));
        parent = node;
      }
      return out;
    },
  });
  return labels.data;
}

/**
 * A file of an encrypted folder, decrypted: shown in a tab when safe, else
 * saved. `keyed` is the node as its keys know it (in a mirror, `sealedAs`).
 */
async function openEncrypted(node: Node, name: string, save: boolean, keyed: Node = node) {
  const type = save ? undefined : inlineType(name);
  // Opened before the wait, or the browser takes it for a popup.
  const tab = type ? window.open("", "_blank") : null;
  try {
    const blob = await decryptedFile(keyed, downloadUrl(node.id));
    if (type && tab) {
      tab.location.href = URL.createObjectURL(new Blob([blob], { type }));
    } else {
      saveBlob(blob, name);
    }
  } catch (error) {
    tab?.close();
    toast.error(`${name}: ${errorMessage(error)}`);
  }
}

/**
 * A folder of a drive: what is in it, and what can be done there. `drive`
 * is the drive whose page it is (My drive while its list loads): the URLs,
 * the crumbs and the move stay in it.
 */
export function FolderView({
  folder,
  drive: driveInfo,
  me,
}: {
  folder: Entry;
  drive: Drive | undefined;
  /** The user's id, to leave a shared drive. */
  me: string;
}) {
  const drive: DriveAt = driveInfo ?? MY_DRIVE;
  const { user } = useRouteContext({ from: "/_app" });
  const children = useQuery(
    listChildrenOptions({ path: { id: folder.id }, query: { limit: 1000 } }),
  );
  const [dialog, setDialog] = useState<
    | { kind: "folder" }
    | { kind: "rename"; node: Node }
    | { kind: "move"; node: Node }
    | { kind: "delete"; node: Node }
    | { kind: "versions"; node: Node }
    | { kind: "share"; node: Node }
    | { kind: "unplace"; node: Node }
    | { kind: "backup"; path: string }
    | { kind: "members" }
    | { kind: "rename-drive" }
    | { kind: "mirror" }
    | { kind: "eject" }
    | undefined
  >();
  const [dragging, setDragging] = useState(false);
  const [uploading, setUploading] = useState(0);
  const [unlocking, setUnlocking] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const refresh = () => queryClient.invalidateQueries();
  // An encrypted folder (or one in it): names and contents are sealed.
  const encrypted = folder.e2e_root != null;
  const unlocked = useUnlocked();
  const locked = encrypted && !unlocked;
  const labels = useShownPath(drive, folder, unlocked);
  const items = children.data?.items ?? [];
  // In a mirror (or a drive ejected from one), or shared from another
  // server, an encrypted folder's keys and names are those where it comes
  // from: its top's `source_id` is the root.
  const elsewhere = sealedElsewhere(folder);
  const sealedTop = useQuery({
    ...getFileOptions({ path: { id: folder.e2e_root ?? "" } }),
    enabled: elsewhere,
  });
  const keyed = (node: Node) => sealedAs(node, folder.source_id, sealedTop.data?.source_id);
  const names = useQuery({
    queryKey: ["shown-names", folder.id, children.dataUpdatedAt, sealedTop.data?.source_id],
    enabled: unlocked && items.some(sealed) && (!elsewhere || sealedTop.data !== undefined),
    queryFn: async () => {
      // Why none can be read, when the keys cannot be had (the owner's
      // server does not answer).
      await openKeys(folder.e2e_root!);
      return new Map(
        await Promise.all(items.map(async (n) => [n.id, await shownName(keyed(n))] as const)),
      );
    },
  });
  const shown = (node: Node) =>
    sealed(node) ? (names.data?.get(node.id) ?? "Encrypted item") : node.name;
  // The user sees decrypted names, sorted as such (folders first).
  const listed = items.some(sealed)
    ? [...items].sort(
        (a, b) =>
          Number(a.kind === "file") - Number(b.kind === "file") || shown(a).localeCompare(shown(b)),
      )
    : items;
  // Its keys for members waiting, or a new key, when it opens here.
  useEffect(() => {
    if (encrypted && unlocked) void seePending();
  }, [encrypted, unlocked, folder.id]);

  const upload = async (files: FileList | File[]) => {
    const list = Array.from(files);
    setUploading((n) => n + list.length);
    for (const file of list) {
      try {
        if (encrypted) await uploadEncryptedFile(folder, file);
        else await uploadFile(folder.id, file);
        toast.success(`Uploaded ${file.name}`);
      } catch (error) {
        toast.error(`${file.name}: ${errorMessage(error)}`);
      } finally {
        setUploading((n) => n - 1);
      }
    }
    void refresh();
  };

  // A drive's root is titled with the drive's name; a shared one's says
  // its members and size, and has the drive's actions.
  const atRoot = !folder.parent_id;
  const title = atRoot
    ? drive.kind === "mirror"
      ? mirrorTitle(drive.name)
      : drive.name
    : (labels?.at(-1) ?? folder.name);
  const shownDrive = atRoot && driveInfo?.kind === "shared" ? driveInfo : undefined;
  // In a share: what it gives; in the user's own files, everything.
  const can = folder.permissions;
  // A drive being moved to other storage is read-only until it is done.
  const moving = driveInfo && movingText(driveInfo);
  // A mirror (docs/M7D_DESIGN.md §5): read-only, with its status, asked
  // again while it copies.
  const isMirror = driveInfo?.kind === "mirror";
  const mirror = useQuery({
    ...getMirrorOptions({ path: { id: driveInfo?.id ?? "" } }),
    enabled: isMirror,
    refetchInterval: (query) =>
      ["copying", "pending"].includes(query.state.data?.status ?? "") ? 3000 : false,
  });
  const banner = isMirror && mirror.data ? statusBanner(mirror.data) : undefined;
  // "Mirror…" at a drive's top, when the user has storage of their own and
  // the drive's managers do not forbid it (the server says the rest).
  const mirrorable = atRoot && (driveInfo?.kind === "my" || driveInfo?.kind === "shared");
  const ownBackends = useQuery({ ...listBackendsOptions(), enabled: mirrorable });
  const driveSettings = useQuery({
    ...listDriveSettingsOptions({ path: { id: driveInfo?.id ?? "" } }),
    enabled: mirrorable && driveInfo?.kind === "shared",
  });
  const allowedSetting = driveSettings.data?.find((s) => s.key === "mirrors.allowed");
  const offerMirror =
    mirrorable &&
    driveInfo !== undefined &&
    (driveInfo.kind !== "my" || can.own) &&
    mayMirror(
      driveInfo,
      ownBackends.data?.length ?? 0,
      allowedSetting ? shownValue(allowedSetting) !== false : undefined,
    );
  // Someone's own storage: its members are warned at the drive's top (a
  // mirror is the user's own, and says so).
  const ownStorage =
    atRoot && driveInfo && !isMirror ? ownStorageWarning(driveInfo, user.email) : undefined;
  // What the user's role here lets them do (docs/M7B_DESIGN.md §1): a
  // viewer reads, upload only adds (and sees only what it added).
  const role = moving && folder.my_role ? "viewer" : folder.my_role;
  const mayAdd = allows(role, "add") && !locked;
  const mounts = children.data?.mounts ?? [];
  const shared = children.data?.shared ?? [];
  return (
    <>
      <PageHeader crumbs={crumbsOf(drive, folder.path, labels)} />
      <div
        className={`flex flex-1 flex-col gap-4 p-4 ${dragging ? "bg-muted" : ""}`}
        onDragOver={(e) => {
          e.preventDefault();
          if (mayAdd) setDragging(true);
        }}
        onDragLeave={() => setDragging(false)}
        onDrop={(e) => {
          e.preventDefault();
          setDragging(false);
          if (e.dataTransfer.files.length && mayAdd) void upload(e.dataTransfer.files);
        }}
      >
        <div className="flex flex-wrap items-center gap-2">
          <div className="min-w-0 flex-1">
            <h1 className="flex items-center gap-2 truncate text-lg font-medium">
              {title}
              {encrypted && (
                <LockIcon
                  className="size-4 shrink-0 text-muted-foreground"
                  aria-label="Encrypted"
                />
              )}
            </h1>
            {shownDrive && (
              <p className="truncate text-sm text-muted-foreground">
                {driveSummary(shownDrive)} · {storedOn(shownDrive)}
              </p>
            )}
            {atRoot && isMirror && mirror.data && (
              <p className="truncate text-sm text-muted-foreground">
                {mirror.data.backend.name} · {lastCopyText(mirror.data)}
              </p>
            )}
            {atRoot && driveInfo?.remote && (
              <p className="truncate text-sm text-muted-foreground">
                From {remoteFrom(driveInfo.remote)}
                {driveInfo.my_role && ` · ${ROLE_LABELS[driveInfo.my_role]}`}
              </p>
            )}
          </div>
          {atRoot && isMirror && mirror.data && mirror.data.status !== "up_to_date" && (
            <MirrorStatusBadge status={mirror.data.status} stopped={mirror.data.stopped} />
          )}
          {offerMirror && (
            <Button variant="outline" onClick={() => setDialog({ kind: "mirror" })}>
              <FolderSyncIcon />
              Mirror…
            </Button>
          )}
          {atRoot && isMirror && (
            <Button
              variant="outline"
              disabled={mirror.data?.status === "copying"}
              onClick={() => setDialog({ kind: "eject" })}
            >
              <HardDriveUploadIcon />
              Eject…
            </Button>
          )}
          {atRoot && isMirror && driveInfo && (
            <Button
              variant="ghost"
              size="icon"
              aria-label="The mirror's trash"
              title="The mirror's trash"
              render={<Link to="/trash" search={{ drive: driveInfo.id }} />}
            >
              <Trash2Icon />
            </Button>
          )}
          {shownDrive && (
            <Button variant="outline" onClick={() => setDialog({ kind: "members" })}>
              <UsersIcon />
              Members…
            </Button>
          )}
          {shownDrive?.manages && (
            <>
              <Button variant="outline" onClick={() => setDialog({ kind: "rename-drive" })}>
                <PencilIcon />
                Rename
              </Button>
              <Button
                variant="outline"
                render={<Link to="/settings/drives/$driveId" params={{ driveId: shownDrive.id }} />}
              >
                <Settings2Icon />
                Settings
              </Button>
            </>
          )}
          {/* Backups are of My drive; a share is in its owner's. */}
          {can.own && drive.kind === "my" && (
            <Button
              variant="ghost"
              size="icon"
              aria-label="From a backup"
              title="From a backup…"
              onClick={() => setDialog({ kind: "backup", path: folder.path })}
            >
              <ArchiveRestoreIcon />
            </Button>
          )}
          {!can.own && !isMirror && (
            <Badge variant="secondary">
              <UsersIcon />
              Shared with you
            </Badge>
          )}
          {/* From another server: its files go through ours (docs/M7E_DESIGN.md §5). */}
          {folder.remote && (
            <Badge variant="outline" title="On another server">
              <GlobeIcon />
              {folder.remote}
            </Badge>
          )}
          {moving || isMirror ? (
            <Badge variant="outline">Read-only</Badge>
          ) : (
            (role === "viewer" || role === "upload_only") && (
              <Badge variant="outline">{ROLE_LABELS[role]}</Badge>
            )
          )}
          {allows(role, "add") && (
            <>
              <Button
                variant="outline"
                disabled={locked}
                onClick={() => setDialog({ kind: "folder" })}
              >
                <FolderPlusIcon />
                New folder
              </Button>
              <Button disabled={locked} onClick={() => input.current?.click()}>
                <UploadIcon />
                {uploading > 0 ? `Uploading ${uploading}…` : "Upload"}
              </Button>
            </>
          )}
          <input
            ref={input}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              if (e.target.files?.length) void upload(e.target.files);
              e.target.value = "";
            }}
          />
        </div>
        {moving && (
          <Alert>
            <LoaderIcon className="animate-spin" />
            <AlertDescription>
              {moving} Until it's done, the drive is read-only: nothing can be added, changed or
              deleted.
            </AlertDescription>
          </Alert>
        )}
        {ownStorage && (
          <Alert>
            <TriangleAlertIcon />
            <AlertDescription>{ownStorage}</AlertDescription>
          </Alert>
        )}
        {atRoot && driveInfo?.kind === "remote" && <RemoteDriveBanner drive={driveInfo} />}
        {banner && (
          <Alert variant={mirror.data?.status === "failing" ? "destructive" : "default"}>
            <TriangleAlertIcon />
            <AlertDescription>{banner}</AlertDescription>
          </Alert>
        )}
        {names.isError && (
          <Alert variant="destructive">
            <TriangleAlertIcon />
            <AlertDescription>{errorMessage(names.error)}</AlertDescription>
          </Alert>
        )}
        {locked ? (
          <EmptyState
            icon={LockIcon}
            title="Unlock to see what's inside"
            action={<Button onClick={() => setUnlocking(true)}>Unlock</Button>}
          >
            Enter your encryption passphrase on this device.
          </EmptyState>
        ) : children.isError ? (
          <p className="text-sm text-destructive">{errorMessage(children.error)}</p>
        ) : children.data && children.data.items.length === 0 ? (
          mayAdd ? (
            <EmptyState icon={UploadIcon} title="Drop files here">
              {role === "upload_only"
                ? "Or upload them. You see only what you added."
                : "Or upload them, or make a folder."}
            </EmptyState>
          ) : (
            <EmptyState icon={FolderOpenIcon} title="Nothing here yet" />
          )
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead className="w-28">Size</TableHead>
                <TableHead className="w-32">Modified</TableHead>
                <TableHead className="w-10" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {listed.map((node) => {
                // A placed share is the user's own entry: renaming or
                // moving it changes only theirs.
                const placed = mounts.includes(node.id);
                const itemRole = node.my_role ?? role;
                const editable = placed ? allows(role, "edit") : allows(itemRole, "edit");
                return (
                  <TableRow key={node.id}>
                    <TableCell className="max-w-0">
                      <ItemLink
                        drive={drive}
                        folder={folder}
                        node={node}
                        keyed={keyed(node)}
                        name={shown(node)}
                        shared={mounts.includes(node.id) || shared.includes(node.id)}
                      />
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {node.kind === "folder" ? "—" : formatSize(shownSize(node))}
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {formatDate(node.updated_at)}
                    </TableCell>
                    <TableCell>
                      <DropdownMenu>
                        <DropdownMenuTrigger
                          render={<Button variant="ghost" size="icon-sm" aria-label="Actions" />}
                        >
                          <MoreHorizontalIcon />
                        </DropdownMenuTrigger>
                        <DropdownMenuContent align="end">
                          {node.kind === "file" && (
                            <>
                              {sealed(node) ? (
                                <DropdownMenuItem
                                  onClick={() =>
                                    void openEncrypted(node, shown(node), true, keyed(node))
                                  }
                                >
                                  <DownloadIcon />
                                  Download
                                </DropdownMenuItem>
                              ) : (
                                <DropdownMenuItem render={<a href={downloadUrl(node.id)} />}>
                                  <DownloadIcon />
                                  Download
                                </DropdownMenuItem>
                              )}
                              {/* Another server keeps no versions here. */}
                              {allows(itemRole, "read") && !node.remote && (
                                <DropdownMenuItem
                                  onClick={() =>
                                    setDialog({
                                      kind: "versions",
                                      // Its keys are the drive mirrored's in a mirror.
                                      node: {
                                        ...node,
                                        name: shown(node),
                                        e2e_root: keyed(node).e2e_root,
                                      },
                                    })
                                  }
                                >
                                  <HistoryIcon />
                                  Versions
                                </DropdownMenuItem>
                              )}
                            </>
                          )}
                          {node.kind === "folder" &&
                            can.own &&
                            drive.kind === "my" &&
                            !mounts.includes(node.id) &&
                            !node.e2e_root && (
                              <DropdownMenuItem
                                onClick={() =>
                                  setDialog({ kind: "backup", path: `${folder.path}/${node.name}` })
                                }
                              >
                                <ArchiveRestoreIcon />
                                From a backup…
                              </DropdownMenuItem>
                            )}
                          {/* An encrypted folder is shared whole, not what is in it; a
                              share from another server is never shared again from here. */}
                          {allows(itemRole, "link") && !sealed(node) && !node.remote && (
                            <DropdownMenuItem onClick={() => setDialog({ kind: "share", node })}>
                              <Share2Icon />
                              Share…
                            </DropdownMenuItem>
                          )}
                          {editable && (
                            <>
                              <DropdownMenuItem
                                onClick={() =>
                                  setDialog({
                                    kind: "rename",
                                    node: { ...node, name: shown(node) },
                                  })
                                }
                              >
                                <PencilIcon />
                                Rename
                              </DropdownMenuItem>
                              <DropdownMenuItem
                                onClick={() =>
                                  setDialog({ kind: "move", node: { ...node, name: shown(node) } })
                                }
                              >
                                <FolderInputIcon />
                                Move
                              </DropdownMenuItem>
                            </>
                          )}
                          {placed ? (
                            <>
                              <DropdownMenuSeparator />
                              <DropdownMenuItem
                                onClick={() => setDialog({ kind: "unplace", node })}
                              >
                                <FolderMinusIcon />
                                Remove from my files
                              </DropdownMenuItem>
                            </>
                          ) : (
                            editable && (
                              <>
                                <DropdownMenuSeparator />
                                <DropdownMenuItem
                                  variant="destructive"
                                  onClick={() =>
                                    setDialog({
                                      kind: "delete",
                                      node: { ...node, name: shown(node) },
                                    })
                                  }
                                >
                                  <Trash2Icon />
                                  Delete
                                </DropdownMenuItem>
                              </>
                            )
                          )}
                        </DropdownMenuContent>
                      </DropdownMenu>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </div>
      {dialog?.kind === "folder" && (
        <NewFolderDialog folder={folder} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "rename" && (
        <NameDialog
          title={`Rename ${dialog.node.name}`}
          action="Rename"
          initial={dialog.node.name}
          onClose={() => setDialog(undefined)}
          mutation={moveFileMutation()}
          variables={async (name) => ({
            path: { id: dialog.node.id },
            body: {
              // Inside an encrypted folder: sealed for the same parent.
              name: sealed(dialog.node) ? await sealedName(folder, name, dialog.node.id) : name,
              if_version: dialog.node.version,
            },
          })}
        />
      )}
      {dialog?.kind === "move" && (
        <MoveDialog
          drive={drive}
          node={dialog.node}
          from={labels ? `/${labels.join("/")}` : folder.path}
          onClose={() => setDialog(undefined)}
        />
      )}
      {unlocking && <UnlockDialog onClose={() => setUnlocking(false)} />}
      {dialog?.kind === "delete" && (
        <DeleteDialog node={dialog.node} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "versions" && (
        <VersionsDialog
          node={dialog.node}
          mirror={isMirror ? mirror.data : undefined}
          mayPurge={
            allows(dialog.node.my_role, "own") ||
            (drive.kind === "shared" && allows(dialog.node.my_role, "manage"))
          }
          onClose={() => setDialog(undefined)}
        />
      )}
      {dialog?.kind === "backup" && (
        <BackupDialog path={dialog.path} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "share" && (
        <ShareDialog node={dialog.node} me={me} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "unplace" && (
        <UnplaceDialog node={dialog.node} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "members" && driveInfo && (
        <DriveAccessDialog drive={driveInfo} me={me} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "mirror" && driveInfo && (
        <MirrorDialog drive={driveInfo} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "eject" && mirror.data && (
        <EjectDialog mirror={mirror.data} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "rename-drive" && driveInfo && (
        <RenameDriveDialog drive={driveInfo} onClose={() => setDialog(undefined)} />
      )}
    </>
  );
}

/** What a file's size is to the user: inside an encrypted folder, its plaintext's. */
function shownSize(node: Node) {
  if (!sealed(node)) return node.size;
  try {
    return plainSize(node.size);
  } catch {
    return node.size;
  }
}

function ItemLink({
  drive,
  folder,
  node,
  keyed,
  name,
  shared,
}: {
  drive: DriveAt;
  folder: Entry;
  node: Node;
  /** The node as its encrypted folder's keys know it (`sealedAs`). */
  keyed: Node;
  /** As the user sees it. */
  name: string;
  shared: boolean;
}) {
  const path = `${folder.path === "/" ? "" : folder.path}/${node.name}`;
  // A share from another server placed here says which server.
  const mark =
    node.remote && !folder.remote ? (
      <span title={`On ${node.remote}`} className="flex shrink-0 items-center">
        <GlobeIcon className="size-3.5 text-muted-foreground" aria-label={`On ${node.remote}`} />
      </span>
    ) : (
      shared && (
        <UsersIcon className="size-3.5 shrink-0 text-muted-foreground" aria-label="Shared" />
      )
    );
  if (node.kind === "folder") {
    return (
      <Link to={driveUrl(drive, path)} className="flex items-center gap-2 truncate font-medium">
        <KindIcon kind="folder" encrypted={node.e2e_root === node.id} />
        <span className="truncate">{name}</span>
        {mark}
      </Link>
    );
  }
  if (sealed(node)) {
    return (
      <button
        type="button"
        className="flex items-center gap-2 truncate text-left"
        onClick={() => void openEncrypted(node, name, false, keyed)}
      >
        <KindIcon kind="file" />
        <span className="truncate">{name}</span>
        {mark}
      </button>
    );
  }
  return (
    <a
      href={downloadUrl(node.id, true)}
      target="_blank"
      rel="noreferrer"
      className="flex items-center gap-2 truncate"
    >
      <KindIcon kind="file" />
      <span className="truncate">{node.name}</span>
      {mark}
    </a>
  );
}

/**
 * A dialog that asks for a name and runs a mutation with it; `variables`
 * may take a while (a name sealed for an encrypted folder).
 */
function NameDialog<D, E, V>({
  title,
  action,
  initial,
  onClose,
  mutation,
  variables,
  children,
}: {
  title: string;
  action: string;
  initial: string;
  onClose: () => void;
  mutation: UseMutationOptions<D, E, V>;
  variables: (name: string) => V | Promise<V>;
  /** More fields, under the name. */
  children?: React.ReactNode;
}) {
  const [name, setName] = useState(initial);
  const [error, setError] = useState<unknown>();
  const run = useMutation({
    ...mutation,
    onSuccess: () => {
      void queryClient.invalidateQueries();
      onClose();
    },
  });
  const submit = async () => {
    setError(undefined);
    try {
      await run.mutateAsync(await variables(name));
    } catch (e) {
      setError(e);
    }
  };
  return (
    <FormDialog
      title={title}
      action={action}
      pending={run.isPending}
      error={error}
      onSubmit={() => void submit()}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="name">Name</FieldLabel>
        <Input
          id="name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
          required
        />
      </Field>
      {children}
    </FormDialog>
  );
}

/**
 * "New folder": in the user's own files (not in an encrypted folder), it
 * may be an encrypted one; inside an encrypted folder its name is sealed.
 */
function NewFolderDialog({ folder, onClose }: { folder: Entry; onClose: () => void }) {
  const [encrypt, setEncrypt] = useState(false);
  // Not where the superadmin or a team turned encrypted folders off.
  const allowed = useMySettings().get("encryption.allowed");
  const canEncrypt =
    folder.permissions.own && folder.e2e_root == null && jsonOf(allowed?.value) !== false;
  return (
    <NameDialog
      title="New folder"
      action="Create folder"
      initial=""
      onClose={onClose}
      mutation={{
        ...createFolderMutation(),
        // A new encrypted folder's first key, made by this device.
        onSettled: () => void seePending(),
      }}
      variables={async (name) => {
        if (folder.e2e_root != null) {
          return { path: { id: folder.id }, body: { name: await sealedName(folder, name) } };
        }
        if (encrypt) {
          if (!(await keyMaterial()).set_up) {
            throw new Error("Set up encryption first, in Settings → Encryption");
          }
          if (!isUnlocked()) throw new Error("Unlock encryption first, in Settings → Encryption");
        }
        return { path: { id: folder.id }, body: { name, encrypted: encrypt } };
      }}
    >
      {canEncrypt && (
        <Field orientation="horizontal">
          <Switch id="encrypt" checked={encrypt} onCheckedChange={setEncrypt} />
          <div className="flex flex-col gap-1">
            <FieldLabel htmlFor="encrypt">Encrypt this folder</FieldLabel>
            <FieldDescription>
              Only you and people you share it with can read it. No AI, search or links.
            </FieldDescription>
          </div>
        </Field>
      )}
    </NameDialog>
  );
}

/**
 * The folder at a path of a drive as the user sees it: walked from the
 * drive's root, through decrypted names inside encrypted folders. A move
 * stays in its drive (the server refuses one between drives).
 */
async function folderAt(drive: DriveAt, path: string): Promise<Node> {
  let at: Node = await driveRoot(drive);
  for (const name of path.split("/").filter(Boolean)) {
    const child = await childNamed(at, name);
    if (!child) throw new Error("There's no folder at that path.");
    at = child;
  }
  return at;
}

/** Move to another folder, given by its path. */
function MoveDialog({
  drive,
  node,
  from,
  onClose,
}: {
  drive: DriveAt;
  node: Node;
  from: string;
  onClose: () => void;
}) {
  const [path, setPath] = useState(from);
  const [error, setError] = useState<string>();
  const move = useMutation({
    ...moveFileMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      onClose();
    },
  });
  const submit = async () => {
    setError(undefined);
    try {
      const target = await folderAt(drive, path);
      if (target.kind !== "folder") {
        setError("That's a file, not a folder.");
        return;
      }
      move.mutate({
        path: { id: node.id },
        body: {
          parent_id: target.id,
          if_version: node.version,
          // Inside an encrypted folder, its name is sealed for its parent.
          ...(sealed(node) ? { name: await sealedName(target, node.name, node.id) } : {}),
        },
      });
    } catch (e) {
      setError(errorMessage(e, "There's no folder at that path."));
    }
  };
  return (
    <FormDialog
      title={`Move ${node.name}`}
      description={
        move.isPending && node.kind === "folder"
          ? "Moving every file in it: on S3 storage that can take a while."
          : undefined
      }
      action={move.isPending ? "Moving…" : "Move"}
      pending={move.isPending}
      error={error ?? move.error}
      onSubmit={() => void submit()}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="path">To the folder</FieldLabel>
        <Input id="path" value={path} onChange={(e) => setPath(e.target.value)} autoFocus />
        <FieldDescription>
          A path from the top of {drive.name}, such as /Documents/Taxes.
        </FieldDescription>
      </Field>
    </FormDialog>
  );
}

function DeleteDialog({ node, onClose }: { node: Node; onClose: () => void }) {
  const remove = useMutation({
    ...deleteFileMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success(`${node.name} is in the trash`);
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  return (
    <ConfirmDialog
      title={`Delete ${node.name}?`}
      description={`It goes to the trash${node.kind === "folder" ? ", with everything in it" : ""}. You can restore it from there.`}
      action="Delete"
      onConfirm={() =>
        remove.mutate({ path: { id: node.id }, query: { if_version: node.version } })
      }
      onClose={onClose}
    />
  );
}

/** A share placed in the user's files goes from them; nothing is deleted. */
function UnplaceDialog({ node, onClose }: { node: Node; onClose: () => void }) {
  const unplace = useMutation({
    ...unplaceSharedMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success(`${node.name} is no longer in your files`);
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  return (
    <ConfirmDialog
      title={`Remove ${node.name} from your files?`}
      description={`It stays shared with you, in "Shared with me", where you can add it again. Nothing of it is deleted.`}
      action="Remove"
      destructive={false}
      onConfirm={() => unplace.mutate({ path: { id: node.id } })}
      onClose={onClose}
    />
  );
}
