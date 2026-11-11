import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import {
  CircleAlertIcon,
  CircleCheckIcon,
  FlaskConicalIcon,
  LockIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from "lucide-react";
import { toast } from "sonner";
import type { Backend } from "organizer-sdk";
import {
  getStorageOptions,
  listGlobalBackendsOptions,
  removeGlobalBackendMutation,
  setStorageMutation,
  testGlobalBackendMutation,
} from "organizer-sdk/react-query";
import { BackendDialog, BackendRow, refreshBackends, testedText } from "@/components/backends";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { LevelSettingsCard } from "@/components/levels";
import { RowMenu } from "@/components/row-menu";
import { SettingsCard } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage, queryClient } from "@/lib/api";

// A superadmin page: a step-up first (`SuperadminGate`).
export const Route = createFileRoute("/_app/settings/storage/")({
  component: () => (
    <SuperadminGate>
      <StoragePage />
    </SuperadminGate>
  ),
});

function StoragePage() {
  const { user } = Route.useRouteContext();
  return (
    <>
      <BackendsCard me={user.id} />
      <FilesFolderCard />
      <LevelSettingsCard
        scope={{ level: "superadmin" }}
        prefixes={["storage."]}
        title="Users' own storage"
        description="Whether users may add S3 storage of their own, for drives they make there. The server doesn't back those up."
      />
    </>
  );
}

/** The server's disk, then the global S3 backends, with Add S3 storage. */
function BackendsCard({ me }: { me: string }) {
  const backends = useQuery(listGlobalBackendsOptions());
  // The dialog: a new backend (null) or one to edit.
  const [editing, setEditing] = useState<Backend | null>();
  const [removing, setRemoving] = useState<Backend>();
  const test = useMutation({
    ...testGlobalBackendMutation(),
    onSuccess: (result) => {
      refreshBackends();
      if (result.ok) toast.success(testedText(result));
      else toast.error(result.error ?? "The connection failed");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const remove = useMutation({
    ...removeGlobalBackendMutation(),
    onSuccess: () => {
      refreshBackends();
      toast.success("Storage removed");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  return (
    <SettingsCard
      title="Storage"
      description="Where shared drives are stored. My drive always stays on the server's disk. The server's backups cover only its disk: for S3 storage, use the provider's versioning or replication."
      action={
        <Button size="sm" variant="outline" onClick={() => setEditing(null)}>
          <PlusIcon />
          Add S3 storage
        </Button>
      }
      error={backends.error}
    >
      <div className="flex flex-col divide-y">
        {backends.data?.map((backend) => (
          <BackendRow
            key={backend.id}
            backend={backend}
            menu={
              backend.kind === "disk" ? (
                <span className="size-7 shrink-0" />
              ) : (
                <RowMenu
                  label={`Actions for ${backend.name}`}
                  actions={[
                    { label: "Edit", icon: <PencilIcon />, onClick: () => setEditing(backend) },
                    {
                      label: "Test connection",
                      icon: <FlaskConicalIcon />,
                      disabled: test.isPending,
                      onClick: () => test.mutate({ path: { id: backend.id } }),
                    },
                    {
                      label: backend.drives > 0 ? "Remove (holds drives)" : "Remove",
                      icon: <Trash2Icon />,
                      destructive: true,
                      disabled: backend.drives > 0,
                      onClick: () => setRemoving(backend),
                    },
                  ]}
                />
              )
            }
          />
        ))}
      </div>
      {editing !== undefined && (
        <BackendDialog
          backend={editing ?? undefined}
          own={false}
          me={me}
          onClose={() => setEditing(undefined)}
        />
      )}
      {removing && (
        <ConfirmDialog
          title={`Remove ${removing.name}?`}
          description="Organizer forgets this storage. Nothing in the bucket is deleted."
          action="Remove"
          onConfirm={() => remove.mutate({ path: { id: removing.id } })}
          onClose={() => setRemoving(undefined)}
        />
      )}
    </SettingsCard>
  );
}

/** My drive's folder on the server's disk. */
function FilesFolderCard() {
  const storage = useQuery(getStorageOptions());
  const [path, setPath] = useState("");
  useEffect(() => {
    if (storage.data) setPath(storage.data.default ? "" : storage.data.files_root);
  }, [storage.data]);
  const save = useMutation({
    ...setStorageMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Saved. The folder is being indexed.");
    },
  });
  const data = storage.data;
  return (
    <SettingsCard
      title="Folder of your files"
      description="On the server's disk. Organizer indexes it where it is and copies nothing. Changing it indexes the new folder from scratch, and the apps list everything again."
      error={storage.error}
    >
      {data && (
        <div className="flex flex-col gap-4">
          <Field>
            <FieldLabel htmlFor="root">Path on the server</FieldLabel>
            <Input
              id="root"
              placeholder={data.default ? data.files_root : "/mnt/nas/files"}
              value={path}
              onChange={(e) => setPath(e.target.value)}
              disabled={data.locked}
            />
            {data.locked ? (
              <FieldDescription>
                <LockIcon className="inline size-3" /> Set by FILES_DIR.
              </FieldDescription>
            ) : (
              <FieldDescription>Empty for the default, in the data folder.</FieldDescription>
            )}
            {save.isError && <FieldError>{errorMessage(save.error)}</FieldError>}
          </Field>
          <p
            className={`flex items-center gap-1 text-sm ${data.available ? "text-muted-foreground" : "text-destructive"}`}
          >
            {data.available ? (
              <CircleCheckIcon className="size-4" />
            ) : (
              <CircleAlertIcon className="size-4" />
            )}
            {data.available ? "Available" : "Not available: is the share mounted?"}
          </p>
          {!data.locked && (
            <Button
              variant="outline"
              className="self-start"
              disabled={save.isPending}
              onClick={() => save.mutate({ body: { files_root: path.trim() || null } })}
            >
              Save
            </Button>
          )}
        </div>
      )}
    </SettingsCard>
  );
}
