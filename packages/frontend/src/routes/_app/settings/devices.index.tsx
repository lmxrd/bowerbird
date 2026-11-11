import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { CodeIcon, MonitorSmartphoneIcon, ServerIcon } from "lucide-react";
import { toast } from "sonner";
import type { ApiKey, CreatedApiKey, KeyKind, Scope } from "organizer-sdk";
import {
  createApiKeyMutation,
  listApiKeysOptions,
  revokeApiKeyMutation,
} from "organizer-sdk/react-query";
import { CopyButton } from "@/components/copy-button";
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

export const Route = createFileRoute("/_app/settings/devices/")({ component: Devices });

const kinds: Record<KeyKind, { label: string; icon: React.ReactNode }> = {
  device: { label: "Device", icon: <MonitorSmartphoneIcon className="size-4" /> },
  app_password: { label: "App password", icon: <ServerIcon className="size-4" /> },
  key: { label: "API key", icon: <CodeIcon className="size-4" /> },
};

function Devices() {
  const keys = useQuery(listApiKeysOptions());
  const [creating, setCreating] = useState<KeyKind>();
  const revoke = useMutation({
    ...revokeApiKeyMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Revoked");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  return (
    <>
      <div className="flex flex-wrap items-center gap-2">
        <div className="flex-1">
          <h2 className="font-medium">Devices and passwords</h2>
          <p className="text-sm text-muted-foreground">
            The apps you signed in, passwords for WebDAV clients, and keys for scripts.
          </p>
        </div>
        <Button variant="outline" onClick={() => setCreating("app_password")}>
          New app password
        </Button>
        <Button variant="outline" onClick={() => setCreating("key")}>
          New API key
        </Button>
      </div>
      {keys.isError && <p className="text-sm text-destructive">{errorMessage(keys.error)}</p>}
      {keys.data?.length === 0 ? (
        <p className="rounded-lg border border-dashed p-6 text-center text-sm text-muted-foreground">
          No devices or passwords yet. Sign in from an app, or make an app password for a WebDAV
          client.
        </p>
      ) : (
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead className="w-32">Last used</TableHead>
              <TableHead className="w-24" />
            </TableRow>
          </TableHeader>
          <TableBody>
            {keys.data?.map((key: ApiKey) => (
              <TableRow key={key.id}>
                <TableCell className="max-w-0">
                  <div className="flex items-center gap-2">
                    <span className="text-muted-foreground">{kinds[key.kind].icon}</span>
                    <div className="min-w-0">
                      <div className="truncate">{key.name}</div>
                      <div className="truncate text-xs text-muted-foreground">
                        {kinds[key.kind].label} · {key.scope} · {key.prefix}…
                      </div>
                    </div>
                  </div>
                </TableCell>
                <TableCell className="text-muted-foreground">
                  {key.last_used_at ? formatDate(key.last_used_at) : "Never"}
                </TableCell>
                <TableCell className="text-right">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => revoke.mutate({ path: { id: key.id } })}
                  >
                    Revoke
                  </Button>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
      {creating && <CreateDialog kind={creating} onClose={() => setCreating(undefined)} />}
    </>
  );
}

function CreateDialog({ kind, onClose }: { kind: KeyKind; onClose: () => void }) {
  const [name, setName] = useState("");
  const [scope, setScope] = useState<Scope>("write");
  const [created, setCreated] = useState<CreatedApiKey>();
  const create = useMutation({
    ...createApiKeyMutation(),
    onSuccess: (key) => {
      setCreated(key);
      void queryClient.invalidateQueries();
    },
  });
  const what = kind === "app_password" ? "app password" : "API key";
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{created ? `Your new ${what}` : `New ${what}`}</DialogTitle>
          <DialogDescription>
            {created
              ? "Copy it now: it isn't shown again."
              : kind === "app_password"
                ? "For a WebDAV client (Finder, Explorer, rclone): any user name, this as the password."
                : "For scripts: send it as Authorization: Bearer."}
          </DialogDescription>
        </DialogHeader>
        {created ? (
          <>
            <code className="rounded-md bg-muted px-2 py-2 font-mono text-sm break-all">
              {created.token}
            </code>
            <DialogFooter>
              <CopyButton text={created.token} />
              <Button onClick={onClose}>Done</Button>
            </DialogFooter>
          </>
        ) : (
          <form
            onSubmit={(e) => {
              e.preventDefault();
              create.mutate({ body: { name, scope, kind } });
            }}
          >
            <FieldGroup>
              <Field>
                <FieldLabel htmlFor="name">Name</FieldLabel>
                <Input
                  id="name"
                  placeholder={kind === "app_password" ? "rclone on the NAS" : "Backup script"}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  autoFocus
                  required
                />
              </Field>
              <Field>
                <FieldLabel>What it may do</FieldLabel>
                <div className="flex gap-2">
                  {(["read", "write"] as Scope[]).map((s) => (
                    <Button
                      key={s}
                      type="button"
                      variant={scope === s ? "secondary" : "outline"}
                      onClick={() => setScope(s)}
                    >
                      {s === "read" ? "Read" : "Read and change"}
                    </Button>
                  ))}
                </div>
                <FieldDescription>
                  Read can only look; Read and change can change your files too. No key reaches the
                  settings of the whole server.
                </FieldDescription>
                {create.isError && <FieldError>{errorMessage(create.error)}</FieldError>}
              </Field>
              <DialogFooter>
                <Button type="button" variant="outline" onClick={onClose}>
                  Cancel
                </Button>
                <Button type="submit" disabled={create.isPending}>
                  Create
                </Button>
              </DialogFooter>
            </FieldGroup>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
