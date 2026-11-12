import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  ChevronDownIcon,
  EyeIcon,
  InboxIcon,
  LinkIcon,
  MoreHorizontalIcon,
  PencilIcon,
  Trash2Icon,
} from "lucide-react";
import { toast } from "sonner";
import type { Link, LinkKind, Node, Role } from "organizer-sdk";
import {
  createLinkMutation,
  deleteLinkMutation,
  listLinksOptions,
  updateLinkMutation,
} from "organizer-sdk/react-query";
import { CopyButton } from "@/components/copy-button";
import { FormDialog } from "@/components/form-dialog";
import { ListRow } from "@/components/list-row";
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
import { errorMessage, queryClient } from "@/lib/api";
import { can } from "@/lib/roles";
import { LINK_KINDS, expiryDate, expiryOf, linkSummary, linkUrl } from "@/lib/sharing";

const KIND_ICONS = { view: EyeIcon, edit: PencilIcon, drop: InboxIcon } as const;

const refresh = () => {
  void queryClient.invalidateQueries({ queryKey: [{ _id: "listLinks" }] });
  void queryClient.invalidateQueries({ queryKey: [{ _id: "listChildren" }] });
};

/**
 * The Share dialog's "Links": public links to the file or folder, each
 * with its kind, password and expiry, its address to copy, and a menu.
 */
export function LinksSection({
  node,
  role,
}: {
  node: Node;
  /** The user's role on it: links need an editor or above (and the settings' leave). */
  role: Role | null | undefined;
}) {
  const links = useQuery(listLinksOptions({ path: { id: node.id } }));
  const [editing, setEditing] = useState<Link | { kind: LinkKind }>();
  const remove = useMutation({
    ...deleteLinkMutation(),
    onSuccess: refresh,
    onError: (error) => toast.error(errorMessage(error)),
  });
  const folder = node.kind === "folder";
  const allowed = {
    view: can(role, "link"),
    edit: folder && can(role, "link"),
    drop: folder && can(role, "link"),
  };
  return (
    <div className="flex min-w-0 flex-col gap-2">
      <div className="flex items-center justify-between">
        <h3 className="text-sm font-medium">Links</h3>
        <DropdownMenu>
          <DropdownMenuTrigger render={<Button variant="outline" size="sm" />}>
            <LinkIcon />
            Add link
            <ChevronDownIcon />
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="w-44">
            {(Object.keys(LINK_KINDS) as LinkKind[]).map((kind) => {
              const Icon = KIND_ICONS[kind];
              return (
                <DropdownMenuItem
                  key={kind}
                  disabled={!allowed[kind]}
                  onClick={() => setEditing({ kind })}
                >
                  <Icon />
                  {LINK_KINDS[kind]}
                </DropdownMenuItem>
              );
            })}
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
      {links.isError && <p className="text-sm text-destructive">{errorMessage(links.error)}</p>}
      {links.data?.length === 0 && (
        <p className="text-sm text-muted-foreground">
          No links: anyone with a link can open it without an account.
        </p>
      )}
      <div className="divide-y">
        {links.data?.map((link) => {
          const Icon = KIND_ICONS[link.kind];
          const url = linkUrl(link.token);
          return (
            <ListRow
              key={link.id}
              icon={<Icon className="size-4 shrink-0 text-muted-foreground" />}
              title={linkSummary(link)}
              detail={url}
            >
              <CopyButton text={url} copied="Link copied" size="sm" />
              <DropdownMenu>
                <DropdownMenuTrigger
                  render={<Button variant="ghost" size="icon-sm" aria-label="Link actions" />}
                  disabled={!link.editable}
                >
                  <MoreHorizontalIcon />
                </DropdownMenuTrigger>
                <DropdownMenuContent align="end" className="w-36">
                  <DropdownMenuItem onClick={() => setEditing(link)}>
                    <PencilIcon />
                    Edit…
                  </DropdownMenuItem>
                  <DropdownMenuSeparator />
                  <DropdownMenuItem
                    variant="destructive"
                    onClick={() => remove.mutate({ path: { id: link.id } })}
                  >
                    <Trash2Icon />
                    Remove
                  </DropdownMenuItem>
                </DropdownMenuContent>
              </DropdownMenu>
            </ListRow>
          );
        })}
      </div>
      {editing && (
        <LinkDialog
          node={node}
          link={"id" in editing ? editing : undefined}
          kind={editing.kind}
          onClose={() => setEditing(undefined)}
        />
      )}
    </div>
  );
}

/** A new link of a kind, or a link's password and expiry. */
function LinkDialog({
  node,
  link,
  kind,
  onClose,
}: {
  node: Node;
  link?: Link;
  kind: LinkKind;
  onClose: () => void;
}) {
  const [password, setPassword] = useState("");
  const [removePassword, setRemovePassword] = useState(false);
  const [expires, setExpires] = useState(expiryDate(link?.expires_at));
  const done = () => {
    refresh();
    onClose();
  };
  const create = useMutation({ ...createLinkMutation(), onSuccess: done });
  const update = useMutation({ ...updateLinkMutation(), onSuccess: done });
  const submit = () => {
    const expires_at = expiryOf(expires);
    if (!link) {
      create.mutate({
        path: { id: node.id },
        body: { kind, password: password || null, expires_at },
      });
      return;
    }
    update.mutate({
      path: { id: link.id },
      body: {
        expires_at,
        ...(password ? { password } : removePassword ? { password: null } : {}),
      },
    });
  };
  return (
    <FormDialog
      title={link ? "Edit link" : `New ${LINK_KINDS[kind].toLowerCase()} link`}
      description={
        kind === "drop"
          ? "Anyone with it can upload files into this folder, and see nothing of it."
          : kind === "edit"
            ? "Anyone with it can see, download, upload, rename and delete in this folder."
            : "Anyone with it can see and download it."
      }
      action={link ? "Save" : "Make link"}
      pending={create.isPending || update.isPending}
      error={create.error ?? update.error}
      onSubmit={submit}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="link-password">Password (optional)</FieldLabel>
        <Input
          id="link-password"
          type="password"
          autoComplete="new-password"
          placeholder={link?.has_password ? "Set: type to replace it" : "None"}
          value={password}
          onChange={(e) => setPassword(e.target.value)}
        />
        <FieldDescription>
          Asked once per browser session.{" "}
          {link?.has_password && !password && (
            <button
              type="button"
              className="underline underline-offset-2"
              onClick={() => setRemovePassword(!removePassword)}
            >
              {removePassword ? "Keep the password" : "Remove the password"}
            </button>
          )}
        </FieldDescription>
      </Field>
      <Field>
        <FieldLabel htmlFor="link-expires">Expires (optional)</FieldLabel>
        <Input
          id="link-expires"
          type="date"
          value={expires}
          onChange={(e) => setExpires(e.target.value)}
        />
        <FieldDescription>It stops working at the end of that day.</FieldDescription>
      </Field>
    </FormDialog>
  );
}
