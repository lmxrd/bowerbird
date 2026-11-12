import { Fragment, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { ChevronRightIcon, FolderIcon } from "lucide-react";
import type { Choice, ItemView } from "organizer-sdk/types";
import { getRootOptions, listChildrenOptions } from "organizer-sdk/react-query";
import {
  Breadcrumb,
  BreadcrumbItem,
  BreadcrumbLink,
  BreadcrumbList,
  BreadcrumbPage,
  BreadcrumbSeparator,
} from "@/components/ui/breadcrumb";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Item, ItemContent, ItemGroup, ItemMedia, ItemTitle } from "@/components/ui/item";
import { errorMessage } from "@/lib/api";

/** A folder of the tree, as the browser goes through it. */
/** A folder browsed to; `encrypted` when it is (in) an end-to-end-encrypted folder. */
export type BrowsedFolder = { id: string; name: string; encrypted?: boolean };

/**
 * Choose where a drop box item goes: a folder of the tree (browsed from the
 * top), a new folder under it if wanted, and the item's name.
 */
export function PlaceDialog({
  item,
  title,
  action,
  pending,
  error,
  onChoose,
  onClose,
}: {
  item: ItemView;
  title: string;
  action: string;
  pending: boolean;
  error: unknown;
  onChoose: (choice: Choice) => void;
  onClose: () => void;
}) {
  // The folders from the top down to the one shown; empty at the top.
  const [trail, setTrail] = useState<BrowsedFolder[]>([]);
  const [newFolder, setNewFolder] = useState("");
  const [name, setName] = useState(item.name);
  const proposal = item.proposal;

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form
          className="flex min-w-0 flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            const folder = newFolder.trim();
            const rename = name.trim();
            onChoose({
              folder_id: trail.at(-1)?.id ?? null,
              new_folders: folder ? [folder] : [],
              name: rename && rename !== item.name ? rename : null,
            });
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {proposal && <DialogDescription>Proposed: {proposal.path}</DialogDescription>}
          </DialogHeader>
          <FieldGroup>
            <Field>
              <FieldLabel>Folder</FieldLabel>
              <FolderBrowser trail={trail} onTrail={setTrail} />
              <FieldDescription>
                It goes into {trail.length === 0 ? "the top" : trail.at(-1)?.name}.
              </FieldDescription>
            </Field>
            <Field>
              <FieldLabel htmlFor="new-folder">New folder (optional)</FieldLabel>
              <Input
                id="new-folder"
                value={newFolder}
                onChange={(e) => setNewFolder(e.target.value)}
              />
              <FieldDescription>Made in the folder above, and the item put in it.</FieldDescription>
            </Field>
            <Field>
              <FieldLabel htmlFor="item-name">Name</FieldLabel>
              <Input
                id="item-name"
                value={name}
                required
                onChange={(e) => setName(e.target.value)}
              />
              {error ? <FieldError>{errorMessage(error)}</FieldError> : null}
            </Field>
          </FieldGroup>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {action}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/**
 * The folders of the tree, browsed from the top: a breadcrumb of the way
 * down (`trail`, empty at the top) and the folders in the one shown.
 */
export function FolderBrowser({
  trail,
  onTrail,
  ownOnly = false,
}: {
  trail: BrowsedFolder[];
  onTrail: (trail: BrowsedFolder[]) => void;
  /** Only the user's own folders: not into the shares they placed. */
  ownOnly?: boolean;
}) {
  const root = useQuery(getRootOptions());
  const current = trail.at(-1)?.id ?? root.data?.id;
  const children = useQuery({
    ...listChildrenOptions({ path: { id: current ?? "" }, query: { limit: 1000 } }),
    enabled: current !== undefined,
  });
  const mounts = ownOnly ? (children.data?.mounts ?? []) : [];
  // A share from another server is never a place to file into (nor to
  // place a share in): the server refuses it.
  const folders =
    children.data?.items.filter(
      (node) => node.kind === "folder" && !mounts.includes(node.id) && !node.remote,
    ) ?? [];
  const setTrail = onTrail;
  return (
    <>
      <Breadcrumb>
        <BreadcrumbList>
          <BreadcrumbItem>
            {trail.length === 0 ? (
              <BreadcrumbPage>Top</BreadcrumbPage>
            ) : (
              <BreadcrumbLink render={<button type="button" />} onClick={() => setTrail([])}>
                Top
              </BreadcrumbLink>
            )}
          </BreadcrumbItem>
          {trail.map((folder, i) => (
            <Fragment key={folder.id}>
              <BreadcrumbSeparator />
              <BreadcrumbItem>
                {i === trail.length - 1 ? (
                  <BreadcrumbPage>{folder.name}</BreadcrumbPage>
                ) : (
                  <BreadcrumbLink
                    render={<button type="button" />}
                    onClick={() => setTrail(trail.slice(0, i + 1))}
                  >
                    {folder.name}
                  </BreadcrumbLink>
                )}
              </BreadcrumbItem>
            </Fragment>
          ))}
        </BreadcrumbList>
      </Breadcrumb>
      <div className="max-h-56 overflow-y-auto rounded-lg border">
        {children.isError ? (
          <p className="p-3 text-sm text-destructive">{errorMessage(children.error)}</p>
        ) : folders.length === 0 ? (
          <p className="p-3 text-sm text-muted-foreground">
            {children.isPending ? "Loading…" : "No folders in here."}
          </p>
        ) : (
          <ItemGroup className="gap-0">
            {folders.map((folder) => (
              <Item
                key={folder.id}
                size="xs"
                render={<button type="button" />}
                className="text-left hover:bg-muted"
                onClick={() =>
                  setTrail([
                    ...trail,
                    { id: folder.id, name: folder.name, encrypted: folder.e2e_root != null },
                  ])
                }
              >
                <ItemMedia variant="icon">
                  <FolderIcon className="text-muted-foreground" />
                </ItemMedia>
                <ItemContent>
                  <ItemTitle>{folder.name}</ItemTitle>
                </ItemContent>
                <ChevronRightIcon className="size-4 text-muted-foreground" />
              </Item>
            ))}
          </ItemGroup>
        )}
      </div>
    </>
  );
}
