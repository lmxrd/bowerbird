import { Fragment, useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import {
  DownloadIcon,
  EyeIcon,
  FolderPlusIcon,
  MoreHorizontalIcon,
  PencilIcon,
  Trash2Icon,
  UploadIcon,
} from "lucide-react";
import { toast } from "sonner";
import type { LinkInfo, Node } from "organizer-sdk";
import {
  createLinkFolderMutation,
  deleteLinkNodeMutation,
  getLinkOptions,
  listLinkChildrenOptions,
  renameLinkNodeMutation,
  unlockLinkMutation,
} from "organizer-sdk/react-query";
import { AuthCard } from "@/components/auth-card";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { EmptyState } from "@/components/empty-state";
import { FormDialog } from "@/components/form-dialog";
import { KindIcon } from "@/components/kind-icon";
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
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Field, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { errorMessage, queryClient } from "@/lib/api";
import { formatDate, formatSize } from "@/lib/format";
import { linkDownloadUrl, linkZipUrl, uploadLinkFile } from "@/lib/transfer";

type Search = { folder?: string };

/** A public link's page: no sign-in. */
export const Route = createFileRoute("/s/$token")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    folder: typeof search.folder === "string" ? search.folder : undefined,
  }),
  component: LinkPage,
});

function LinkPage() {
  const { token } = Route.useParams();
  const info = useQuery(getLinkOptions({ path: { token } }));
  if (info.isPending) {
    return (
      <AuthCard title="Organizer">
        <Skeleton className="h-24" />
      </AuthCard>
    );
  }
  if (info.isError) {
    return (
      <AuthCard title="This link does not work">
        <p className="text-sm text-muted-foreground">{errorMessage(info.error)}</p>
      </AuthCard>
    );
  }
  const link = info.data;
  if (link.locked) return <Unlock token={token} />;
  if (link.kind === "drop") return <FileDrop token={token} link={link} />;
  if (link.node?.kind === "file") return <SharedFile token={token} node={link.node} />;
  return <SharedFolder token={token} link={link} />;
}

/** The password, asked once per browser session. */
function Unlock({ token }: { token: string }) {
  const [password, setPassword] = useState("");
  const unlock = useMutation({
    ...unlockLinkMutation(),
    onSuccess: () => void queryClient.invalidateQueries(),
  });
  return (
    <AuthCard title="This link has a password" description="Ask whoever sent it to you.">
      <form
        className="flex flex-col gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          unlock.mutate({ path: { token }, body: { password } });
        }}
      >
        <Field>
          <FieldLabel htmlFor="password">Password</FieldLabel>
          <Input
            id="password"
            type="password"
            autoFocus
            required
            value={password}
            onChange={(e) => setPassword(e.target.value)}
          />
          {unlock.error ? <FieldError>{errorMessage(unlock.error)}</FieldError> : null}
        </Field>
        <Button type="submit" disabled={unlock.isPending}>
          Open
        </Button>
      </form>
    </AuthCard>
  );
}

/** A file on its own: preview and download. */
function SharedFile({ token, node }: { token: string; node: Node }) {
  return (
    <AuthCard
      title={
        <span className="flex items-center gap-2">
          <KindIcon kind="file" />
          <span className="truncate">{node.name}</span>
        </span>
      }
      description={`${formatSize(node.size)} · changed ${formatDate(node.updated_at)}`}
    >
      <div className="flex gap-2">
        <Button
          variant="outline"
          className="flex-1"
          render={
            <a href={linkDownloadUrl(token, node.id, true)} target="_blank" rel="noreferrer" />
          }
        >
          <EyeIcon />
          Preview
        </Button>
        <Button className="flex-1" render={<a href={linkDownloadUrl(token, node.id)} />}>
          <DownloadIcon />
          Download
        </Button>
      </div>
    </AuthCard>
  );
}

/** A file drop: upload only, nothing of the folder shown. */
function FileDrop({ token, link }: { token: string; link: LinkInfo }) {
  const [sent, setSent] = useState<string[]>([]);
  const [uploading, setUploading] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const upload = useUpload(token, undefined, (name) => setSent((s) => [...s, name]), setUploading);
  return (
    <AuthCard
      title={`Send files to ${link.name ?? "this folder"}`}
      description="What you upload goes to the owner of this link. You won't see the folder's files."
    >
      <div
        className="flex flex-col items-center gap-3 rounded-lg border border-dashed p-6 text-center"
        onDragOver={(e) => e.preventDefault()}
        onDrop={(e) => {
          e.preventDefault();
          if (e.dataTransfer.files.length) void upload(e.dataTransfer.files);
        }}
      >
        <UploadIcon className="size-6 text-muted-foreground" />
        <p className="text-sm text-muted-foreground">Drop files here, or</p>
        <Button onClick={() => input.current?.click()} disabled={uploading > 0}>
          {uploading > 0 ? `Uploading ${uploading}…` : "Choose files"}
        </Button>
        <FileInput input={input} onFiles={upload} />
      </div>
      {sent.length > 0 && (
        <ul className="text-sm text-muted-foreground">
          {sent.map((name, i) => (
            <li key={`${name}-${i}`} className="truncate">
              Sent {name}
            </li>
          ))}
        </ul>
      )}
    </AuthCard>
  );
}

/** Upload a list of files through the link, one by one, saying how each went. */
function useUpload(
  token: string,
  folder: string | undefined,
  done: (name: string) => void,
  setUploading: (update: (n: number) => number) => void,
) {
  return async (files: FileList | File[]) => {
    const list = Array.from(files);
    setUploading((n) => n + list.length);
    for (const file of list) {
      try {
        done(await uploadLinkFile(token, file, folder));
        toast.success(`Uploaded ${file.name}`);
      } catch (error) {
        toast.error(`${file.name}: ${errorMessage(error)}`);
      } finally {
        setUploading((n) => n - 1);
      }
    }
  };
}

function FileInput({
  input,
  onFiles,
}: {
  input: React.RefObject<HTMLInputElement | null>;
  onFiles: (files: FileList) => void;
}) {
  return (
    <input
      ref={input}
      type="file"
      multiple
      hidden
      onChange={(e) => {
        if (e.target.files?.length) onFiles(e.target.files);
        e.target.value = "";
      }}
    />
  );
}

type Dialog = { kind: "folder" } | { kind: "rename"; node: Node } | { kind: "delete"; node: Node };

/** A folder: its listing, downloads, and (an edit link) changes in it. */
function SharedFolder({ token, link }: { token: string; link: LinkInfo }) {
  const { folder } = Route.useSearch();
  const children = useQuery(listLinkChildrenOptions({ path: { token }, query: { folder } }));
  const [dialog, setDialog] = useState<Dialog>();
  const [uploading, setUploading] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const edit = link.kind === "edit";
  const current = children.data?.folder;
  const upload = useUpload(token, current?.id, refreshLinkFolder, setUploading);
  const trail = children.data?.trail ?? [];
  return (
    <div className="mx-auto flex min-h-svh w-full max-w-4xl flex-col gap-4 p-6">
      <div className="flex flex-wrap items-center gap-2">
        <Breadcrumb className="flex-1">
          <BreadcrumbList>
            {trail.map((node, i) => (
              <Fragment key={node.id}>
                {i > 0 && <BreadcrumbSeparator />}
                <BreadcrumbItem>
                  {i === trail.length - 1 ? (
                    <BreadcrumbPage className="text-lg font-medium">{node.name}</BreadcrumbPage>
                  ) : (
                    <BreadcrumbLink
                      render={
                        <Link
                          to="/s/$token"
                          params={{ token }}
                          search={{ folder: i === 0 ? undefined : node.id }}
                        />
                      }
                    >
                      {node.name}
                    </BreadcrumbLink>
                  )}
                </BreadcrumbItem>
              </Fragment>
            ))}
          </BreadcrumbList>
        </Breadcrumb>
        {current && (
          <Button variant="outline" render={<a href={linkZipUrl(token, current.id)} />}>
            <DownloadIcon />
            Download all
          </Button>
        )}
        {edit && (
          <>
            <Button variant="outline" onClick={() => setDialog({ kind: "folder" })}>
              <FolderPlusIcon />
              New folder
            </Button>
            <Button onClick={() => input.current?.click()}>
              <UploadIcon />
              {uploading > 0 ? `Uploading ${uploading}…` : "Upload"}
            </Button>
            <FileInput input={input} onFiles={upload} />
          </>
        )}
      </div>
      {children.isError ? (
        <p className="text-sm text-destructive">{errorMessage(children.error)}</p>
      ) : children.data && children.data.items.length === 0 ? (
        <EmptyState icon={UploadIcon} title="Nothing in here">
          {edit ? "Upload files, or make a folder." : "This folder is empty."}
        </EmptyState>
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
            {children.data?.items.map((node) => (
              <TableRow key={node.id}>
                <TableCell className="max-w-0">
                  {node.kind === "folder" ? (
                    <Link
                      to="/s/$token"
                      params={{ token }}
                      search={{ folder: node.id }}
                      className="flex items-center gap-2 truncate font-medium"
                    >
                      <KindIcon kind="folder" />
                      <span className="truncate">{node.name}</span>
                    </Link>
                  ) : (
                    <a
                      href={linkDownloadUrl(token, node.id, true)}
                      target="_blank"
                      rel="noreferrer"
                      className="flex items-center gap-2 truncate"
                    >
                      <KindIcon kind="file" />
                      <span className="truncate">{node.name}</span>
                    </a>
                  )}
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {node.kind === "folder" ? "—" : formatSize(node.size)}
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
                      <DropdownMenuItem
                        render={
                          <a
                            href={
                              node.kind === "file"
                                ? linkDownloadUrl(token, node.id)
                                : linkZipUrl(token, node.id)
                            }
                          />
                        }
                      >
                        <DownloadIcon />
                        {node.kind === "file" ? "Download" : "Download as zip"}
                      </DropdownMenuItem>
                      {edit && (
                        <>
                          <DropdownMenuItem onClick={() => setDialog({ kind: "rename", node })}>
                            <PencilIcon />
                            Rename
                          </DropdownMenuItem>
                          <DropdownMenuSeparator />
                          <DropdownMenuItem
                            variant="destructive"
                            onClick={() => setDialog({ kind: "delete", node })}
                          >
                            <Trash2Icon />
                            Delete
                          </DropdownMenuItem>
                        </>
                      )}
                    </DropdownMenuContent>
                  </DropdownMenu>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {dialog?.kind === "folder" && current && (
        <NewFolderDialog token={token} parent={current.id} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "rename" && (
        <RenameDialog token={token} node={dialog.node} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "delete" && (
        <DeleteDialog token={token} node={dialog.node} onClose={() => setDialog(undefined)} />
      )}
    </div>
  );
}

const refreshLinkFolder = () =>
  void queryClient.invalidateQueries({ queryKey: [{ _id: "listLinkChildren" }] });

function NewFolderDialog({
  token,
  parent,
  onClose,
}: {
  token: string;
  parent: string;
  onClose: () => void;
}) {
  const [name, setName] = useState("");
  const create = useMutation({
    ...createLinkFolderMutation(),
    onSuccess: () => {
      refreshLinkFolder();
      onClose();
    },
  });
  return (
    <FormDialog
      title="New folder"
      action="Create folder"
      pending={create.isPending}
      error={create.error}
      onSubmit={() => create.mutate({ path: { token }, body: { parent_id: parent, name } })}
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
    </FormDialog>
  );
}

function RenameDialog({
  token,
  node,
  onClose,
}: {
  token: string;
  node: Node;
  onClose: () => void;
}) {
  const [name, setName] = useState(node.name);
  const rename = useMutation({
    ...renameLinkNodeMutation(),
    onSuccess: () => {
      refreshLinkFolder();
      onClose();
    },
  });
  return (
    <FormDialog
      title={`Rename ${node.name}`}
      action="Rename"
      pending={rename.isPending}
      error={rename.error}
      onSubmit={() =>
        rename.mutate({
          path: { token, id: node.id },
          body: { name, if_version: node.version },
        })
      }
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
    </FormDialog>
  );
}

function DeleteDialog({
  token,
  node,
  onClose,
}: {
  token: string;
  node: Node;
  onClose: () => void;
}) {
  const remove = useMutation({
    ...deleteLinkNodeMutation(),
    onSuccess: () => {
      refreshLinkFolder();
      toast.success(`Deleted ${node.name}`);
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  return (
    <ConfirmDialog
      title={`Delete ${node.name}?`}
      description="It goes to the owner's trash."
      action="Delete"
      onConfirm={() =>
        remove.mutate({ path: { token, id: node.id }, query: { if_version: node.version } })
      }
      onClose={onClose}
    />
  );
}
