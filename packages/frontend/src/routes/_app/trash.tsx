import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { DownloadIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import type { TrashItem } from "organizer-sdk";
import {
  emptyTrashMutation,
  getRetentionOptions,
  listTrashOptions,
  purgeFromTrashMutation,
  restoreFromMirrorTrashMutation,
  restoreFromTrashMutation,
} from "organizer-sdk/react-query";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { mirrorError } from "@/components/mirrors";
import { EmptyState } from "@/components/empty-state";
import { KindIcon } from "@/components/kind-icon";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage, queryClient } from "@/lib/api";
import { drivesOptions, myDrive } from "@/lib/drive-files";
import { shownName, useUnlocked } from "@/lib/encryption";
import { formatDate, formatGoes, formatSize } from "@/lib/format";
import { mirrorTitle, restoreAction, trashSealedAs } from "@/lib/mirrors";
import { trashActions } from "@/lib/roles";
import { mirrorTrashUrl } from "@/lib/transfer";

/**
 * The trash of one drive (each drive has its own): `?drive=<id>`, My
 * drive's without it. The sidebar's link carries the drive the user is in.
 */
export const Route = createFileRoute("/_app/trash")({
  validateSearch: (search: Record<string, unknown>): { drive?: string } =>
    typeof search.drive === "string" && search.drive ? { drive: search.drive } : {},
  component: Trash,
});

function Trash() {
  const { drive } = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const drives = useQuery(drivesOptions());
  const my = myDrive(drives.data);
  const query = drive ? { drive } : {};
  const trash = useQuery(listTrashOptions({ query }));
  const retention = useQuery(getRetentionOptions());
  const days = retention.data?.rules.trash_days ?? 0;
  const [confirm, setConfirm] = useState<{ kind: "purge"; item: TrashItem } | { kind: "empty" }>();
  const done = (message: string) => () => {
    void queryClient.invalidateQueries();
    toast.success(message);
  };
  const failed = (error: unknown) => toast.error(errorMessage(error));
  const restore = useMutation({
    ...restoreFromTrashMutation(),
    onSuccess: done("Restored"),
    onError: failed,
  });
  // A mirror's trash (docs/M7D_DESIGN.md §9): restoring copies the item
  // back into the drive mirrored, or a file is downloaded instead.
  const mirror = drives.data?.find((d) => d.id === drive && d.kind === "mirror")?.mirror_of;
  const restoreMirror = useMutation({
    ...restoreFromMirrorTrashMutation(),
    onSuccess: done(`Restored into ${mirror?.drive_name ?? "the drive"}`),
    onError: (error) => toast.error(mirrorError(error)),
  });
  const purge = useMutation({
    ...purgeFromTrashMutation(),
    onSuccess: done("Deleted for good"),
    onError: failed,
  });
  const empty = useMutation({
    ...emptyTrashMutation(),
    onSuccess: done("The trash is empty"),
    onError: failed,
  });
  const items = trash.data ?? [];
  // Each item says what the user may do with it (their role on the drive).
  const mayEmpty = items.length > 0 && items.every((item) => trashActions(item.my_role).purge);
  return (
    <>
      <PageHeader crumbs={[{ label: "Trash" }]} />
      <div className="flex flex-1 flex-col gap-4 p-4">
        <div className="flex items-center gap-2">
          <div className="flex-1">
            <h1 className="text-lg font-medium">Trash</h1>
            {mirror && (
              <p className="text-sm text-muted-foreground">
                What was deleted in {mirror.drive_name} since it was mirrored.{" "}
                {mirror.restores_into_drive
                  ? "Restoring copies it back there."
                  : "You can't write there: download what you need."}
              </p>
            )}
            {retention.data && (
              <p className="text-sm text-muted-foreground">
                {days > 0
                  ? `Items are deleted for good ${days} days after you delete them. `
                  : "Items stay until you empty the trash. "}
                <Link to="/settings/retention" className="underline underline-offset-4">
                  Change
                </Link>
              </p>
            )}
          </div>
          {/* Which drive's trash, when the user has more than My drive. */}
          {drives.data && drives.data.length > 1 && (
            <Select
              items={drives.data.map((d) => ({
                value: d.id,
                label: d.kind === "mirror" ? mirrorTitle(d.name) : d.name,
              }))}
              value={drive ?? my?.id ?? null}
              onValueChange={(id) =>
                void navigate({ search: id && id !== my?.id ? { drive: id } : {} })
              }
            >
              <SelectTrigger aria-label="Drive" className="w-48">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {drives.data.map((d) => (
                  <SelectItem key={d.id} value={d.id}>
                    {d.kind === "mirror" ? mirrorTitle(d.name) : d.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          )}
          {mayEmpty && (
            <Button variant="destructive" onClick={() => setConfirm({ kind: "empty" })}>
              Empty trash
            </Button>
          )}
        </div>
        {trash.isError && <p className="text-sm text-destructive">{errorMessage(trash.error)}</p>}
        {trash.data?.length === 0 ? (
          <EmptyState icon={Trash2Icon} title="The trash is empty">
            What you delete comes here first, and can be restored.
          </EmptyState>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead className="w-28">Size</TableHead>
                <TableHead className="w-28">Deleted</TableHead>
                {days > 0 && <TableHead className="w-32">Goes</TableHead>}
                <TableHead className="w-64" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((item) => {
                const may = trashActions(item.my_role);
                const fromMirror = mirror && drive ? restoreAction(mirror, item.kind) : undefined;
                return (
                  <TableRow key={item.id}>
                    <TableCell className="max-w-0">
                      <div className="flex items-center gap-2">
                        <KindIcon kind={item.kind} encrypted={item.e2e_root === item.id} />
                        <TrashName item={item} />
                      </div>
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {item.kind === "folder" ? "—" : formatSize(item.size)}
                    </TableCell>
                    <TableCell className="text-muted-foreground">
                      {formatDate(item.deleted_at)}
                    </TableCell>
                    {days > 0 && (
                      <TableCell className="text-muted-foreground">
                        {formatGoes(item.deleted_at, days)}
                      </TableCell>
                    )}
                    <TableCell className="text-right">
                      <div className="flex justify-end gap-2">
                        {fromMirror === "restore" && drive && (
                          <Button
                            variant="outline"
                            size="sm"
                            title={`Into ${mirror?.drive_name}`}
                            disabled={restoreMirror.isPending}
                            onClick={() =>
                              restoreMirror.mutate({ path: { id: drive, item_id: item.id } })
                            }
                          >
                            Restore
                          </Button>
                        )}
                        {fromMirror === "download" && drive && (
                          <Button
                            variant="outline"
                            size="sm"
                            render={<a href={mirrorTrashUrl(drive, item.id)} />}
                          >
                            <DownloadIcon />
                            Download
                          </Button>
                        )}
                        {may.restore && (
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => restore.mutate({ path: { id: item.id } })}
                          >
                            Restore
                          </Button>
                        )}
                        {may.purge && (
                          <Button
                            variant="destructive"
                            size="sm"
                            onClick={() => setConfirm({ kind: "purge", item })}
                          >
                            Delete forever
                          </Button>
                        )}
                      </div>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </div>
      {confirm?.kind === "purge" && (
        <ConfirmDialog
          title={`Delete ${confirm.item.name} forever?`}
          description="This can't be undone."
          action="Delete forever"
          onConfirm={() => purge.mutate({ path: { id: confirm.item.id } })}
          onClose={() => setConfirm(undefined)}
        />
      )}
      {confirm?.kind === "empty" && (
        <ConfirmDialog
          title="Empty the trash?"
          description={`All ${items.length} items are deleted for good, with their earlier versions. This can't be undone.`}
          action="Empty trash"
          onConfirm={() => empty.mutate({ query })}
          onClose={() => setConfirm(undefined)}
        />
      )}
    </>
  );
}

/**
 * A trashed item's name and where it was: inside an encrypted folder, its
 * name decrypted once unlocked (sealed with the folder it was in).
 */
function TrashName({ item }: { item: TrashItem }) {
  const unlocked = useUnlocked();
  const inside = item.e2e_root != null && item.e2e_root !== item.id;
  const name = useQuery({
    queryKey: ["trash-name", item.id],
    enabled: inside && unlocked,
    // In a mirror, sealed with the drive's ids.
    queryFn: () => shownName(trashSealedAs(item)),
  });
  return (
    <div className="min-w-0">
      <div className="truncate">{inside ? (name.data ?? "Encrypted item") : item.name}</div>
      <div className="truncate text-xs text-muted-foreground">
        {inside ? "In an encrypted folder" : `/${item.original_path}`}
      </div>
    </div>
  );
}
