import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import {
  CheckIcon,
  FolderInputIcon,
  FolderMinusIcon,
  GlobeIcon,
  Share2Icon,
  XIcon,
} from "lucide-react";
import { toast } from "sonner";
import type { SharedItem } from "organizer-sdk";
import {
  acceptSharedMutation,
  declineSharedMutation,
  getRootOptions,
  markSharedSeenMutation,
  placeSharedMutation,
  unplaceSharedMutation,
} from "organizer-sdk/react-query";
import { EmptyState } from "@/components/empty-state";
import { FormDialog } from "@/components/form-dialog";
import { KindIcon } from "@/components/kind-icon";
import { PageHeader } from "@/components/page-header";
import { type BrowsedFolder, FolderBrowser } from "@/components/place-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage, queryClient, sharedWithMeOptions } from "@/lib/api";
import { folderUrl } from "@/lib/format";
import { ROLE_LABELS } from "@/lib/roles";
import { sharedFrom } from "@/lib/sharing";

export const Route = createFileRoute("/_app/shared")({ component: SharedWithMe });

/** "Shared with me": what others shared, and where the user put it. */
function SharedWithMe() {
  const shared = useQuery(sharedWithMeOptions());
  const [placing, setPlacing] = useState<SharedItem>();
  const refresh = () => queryClient.invalidateQueries();
  const failed = (error: unknown) => toast.error(errorMessage(error));
  const seen = useMutation({
    ...markSharedSeenMutation(),
    onSuccess: () =>
      void queryClient.invalidateQueries({ queryKey: [{ _id: "listSharedWithMe" }] }),
  });
  const unplace = useMutation({
    ...unplaceSharedMutation(),
    onSuccess: () => {
      void refresh();
      toast.success("Removed from your files");
    },
    onError: failed,
  });
  const decline = useMutation({
    ...declineSharedMutation(),
    onSuccess: () => {
      void refresh();
      toast.success("Declined");
    },
    onError: failed,
  });
  const accept = useMutation({
    ...acceptSharedMutation(),
    onSuccess: () => {
      void refresh();
      toast.success("Accepted");
    },
    onError: failed,
  });
  // Looked at: what is here is no longer new (the sidebar's badge).
  const hasNew = shared.data?.some((item) => item.new) ?? false;
  const markSeen = seen.mutate;
  useEffect(() => {
    if (hasNew) markSeen({});
  }, [hasNew, markSeen]);

  const items = shared.data ?? [];
  return (
    <>
      <PageHeader crumbs={[{ label: "Shared with me" }]} />
      <div className="flex flex-1 flex-col gap-4 p-4">
        <h1 className="text-lg font-medium">Shared with me</h1>
        {shared.isError ? (
          <p className="text-sm text-destructive">{errorMessage(shared.error)}</p>
        ) : shared.data && items.length === 0 ? (
          <EmptyState icon={Share2Icon} title="Nothing is shared with you">
            What others share with you, or with a group you are in, shows here, and what people on
            other servers share with you.
          </EmptyState>
        ) : (
          <Table>
            {/* Name takes what the others leave: From and Role are narrow, and
                go below the name where the page is narrow (md, xl); the
                actions are as wide as their buttons. */}
            <TableHeader>
              <TableRow>
                <TableHead>Name</TableHead>
                <TableHead className="hidden w-44 md:table-cell 2xl:w-56">From</TableHead>
                <TableHead className="hidden w-28 xl:table-cell">Role</TableHead>
                <TableHead className="w-px" />
              </TableRow>
            </TableHeader>
            <TableBody>
              {items.map((item) => (
                <TableRow key={item.node.id}>
                  <TableCell className="max-w-0">
                    <div className="flex items-center gap-2">
                      <KindIcon kind={item.node.kind} />
                      <span className="truncate font-medium">{item.node.name}</span>
                      {item.new && <Badge>New</Badge>}
                    </div>
                    {/* The columns hidden at this width. */}
                    <p className="truncate text-xs text-muted-foreground xl:hidden">
                      <span className="md:hidden">{sharedFrom(item)} · </span>
                      {ROLE_LABELS[item.role]}
                    </p>
                    {item.remote?.state === "pending" && (
                      <p className="truncate text-xs text-muted-foreground">
                        From another server: accept it to keep it.
                      </p>
                    )}
                    {item.remote?.error && (
                      <p
                        className="truncate text-xs text-destructive"
                        title={`${item.remote.server} isn't reachable now: ${item.remote.error}`}
                      >
                        {item.remote.server} isn't reachable now: {item.remote.error}
                      </p>
                    )}
                    {item.placed && (
                      <p className="truncate text-xs text-muted-foreground">
                        In your files at{" "}
                        <Link
                          className="underline underline-offset-2"
                          to={
                            item.node.kind === "folder"
                              ? folderUrl(item.placed)
                              : folderUrl(item.placed.split("/").slice(0, -1).join("/"))
                          }
                        >
                          {item.placed}
                        </Link>
                      </p>
                    )}
                  </TableCell>
                  <TableCell className="hidden max-w-0 text-muted-foreground md:table-cell">
                    {item.remote ? (
                      // From another server (docs/M7E_DESIGN.md §5).
                      <span className="flex items-center gap-1.5" title={sharedFrom(item)}>
                        <GlobeIcon className="size-3.5 shrink-0" />
                        <span className="truncate">{sharedFrom(item)}</span>
                      </span>
                    ) : (
                      <span className="block truncate">{sharedFrom(item)}</span>
                    )}
                  </TableCell>
                  <TableCell className="hidden text-muted-foreground xl:table-cell">
                    {ROLE_LABELS[item.role]}
                  </TableCell>
                  <TableCell>
                    <div className="flex justify-end gap-2">
                      {item.placed ? (
                        <Button
                          variant="outline"
                          size="sm"
                          onClick={() => unplace.mutate({ path: { id: item.node.id } })}
                        >
                          <FolderMinusIcon />
                          Remove from my files
                        </Button>
                      ) : (
                        <>
                          {/* A share from another server waits to be accepted; once
                              it is, it can be added to the user's files. */}
                          {item.remote?.state === "pending" ? (
                            <Button
                              size="sm"
                              disabled={accept.isPending}
                              onClick={() => accept.mutate({ path: { id: item.node.id } })}
                            >
                              <CheckIcon />
                              Accept
                            </Button>
                          ) : (
                            <Button variant="outline" size="sm" onClick={() => setPlacing(item)}>
                              <FolderInputIcon />
                              Add to my files…
                            </Button>
                          )}
                          <Button
                            variant="ghost"
                            size="sm"
                            onClick={() => decline.mutate({ path: { id: item.node.id } })}
                          >
                            <XIcon />
                            Decline
                          </Button>
                        </>
                      )}
                    </div>
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        )}
      </div>
      {placing && <AddToFilesDialog item={placing} onClose={() => setPlacing(undefined)} />}
    </>
  );
}

/** "Add to my files…": a folder of the user's own files, and its name there. */
function AddToFilesDialog({ item, onClose }: { item: SharedItem; onClose: () => void }) {
  const [trail, setTrail] = useState<BrowsedFolder[]>([]);
  const [name, setName] = useState(item.node.name);
  const root = useQuery(getRootOptions());
  const place = useMutation({
    ...placeSharedMutation(),
    onSuccess: (entry) => {
      void queryClient.invalidateQueries();
      toast.success(`Added to your files at ${entry.path}`);
      onClose();
    },
  });
  const parent = trail.at(-1);
  return (
    <FormDialog
      title={`Add ${item.node.name} to my files`}
      description="It shows as a folder of yours, synced and in WebDAV like your own; it stays its owner's."
      action="Add"
      pending={place.isPending}
      error={place.error}
      onSubmit={() =>
        place.mutate({
          path: { id: item.node.id },
          body: {
            parent_id: parent?.id ?? root.data?.id ?? "",
            name: name.trim() === item.node.name ? null : name.trim(),
          },
        })
      }
      onClose={onClose}
    >
      <Field>
        <FieldLabel>Folder</FieldLabel>
        <FolderBrowser trail={trail} onTrail={setTrail} ownOnly />
        <FieldDescription>It goes into {parent ? parent.name : "the top"}.</FieldDescription>
      </Field>
      <Field>
        <FieldLabel htmlFor="place-name">Name</FieldLabel>
        <Input id="place-name" value={name} required onChange={(e) => setName(e.target.value)} />
      </Field>
    </FormDialog>
  );
}
