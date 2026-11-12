import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import {
  CloudIcon,
  FlaskConicalIcon,
  LockIcon,
  LockOpenIcon,
  PencilIcon,
  PlusIcon,
  Trash2Icon,
} from "lucide-react";
import { toast } from "sonner";
import type { Backend } from "organizer-sdk";
import {
  listBackendsOptions,
  lockBackendMutation,
  removeBackendMutation,
  testBackendMutation,
} from "organizer-sdk/react-query";
import { BackendDialog, BackendRow, refreshBackends, testedText } from "@/components/backends";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { NewDriveDialog } from "@/components/drive-dialogs";
import { EmptyState } from "@/components/empty-state";
import { useMySettings } from "@/components/levels";
import { type RowAction, RowMenu } from "@/components/row-menu";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { canUnlock, unlockBackendWithKey, useUnlocked } from "@/lib/encryption";

/**
 * Settings → Storage → Your storage (docs/M7C_DESIGN.md §2): a user's own
 * S3 storage for drives they make there, when the superadmin (or a team)
 * allows it (`storage.own_backends`).
 */
export const Route = createFileRoute("/_app/settings/storage/yours")({ component: YourStorage });

function YourStorage() {
  const { user } = Route.useRouteContext();
  const mine = useMySettings();
  const allowed = mine.get("storage.own_backends")?.value === true;
  const backends = useQuery(listBackendsOptions());
  const unlocked = useUnlocked();
  // The dialog: a new backend (null) or one to edit.
  const [editing, setEditing] = useState<Backend | null>();
  const [removing, setRemoving] = useState<Backend>();
  const [newDrive, setNewDrive] = useState(false);
  const failed = (error: unknown) => toast.error(errorMessage(error));
  const test = useMutation({
    ...testBackendMutation(),
    onSuccess: (result) => {
      refreshBackends();
      if (result.ok) toast.success(testedText(result));
      else toast.error(result.error ?? "The connection failed");
    },
    onError: failed,
  });
  const lock = useMutation({
    ...lockBackendMutation(),
    onSuccess: () => {
      refreshBackends();
      toast.success("Locked in this browser");
    },
    onError: failed,
  });
  const unlock = useMutation({
    mutationFn: unlockBackendWithKey,
    onSuccess: () => {
      refreshBackends();
      toast.success("Unlocked");
    },
    onError: failed,
  });
  const remove = useMutation({
    ...removeBackendMutation(),
    onSuccess: () => {
      refreshBackends();
      toast.success("Storage removed");
    },
    onError: failed,
  });
  const list = backends.data ?? [];
  if (backends.data && !allowed && list.length === 0) {
    return (
      <EmptyState icon={CloudIcon} title="No storage of your own">
        Your administrator hasn't allowed adding your own storage.
      </EmptyState>
    );
  }
  const actions = (backend: Backend): RowAction[] => [
    ...(backend.private && backend.status === "locked"
      ? [
          {
            label: "Unlock",
            icon: <LockOpenIcon />,
            disabled: !unlocked || !canUnlock(backend) || unlock.isPending,
            onClick: () => unlock.mutate(backend),
          },
        ]
      : []),
    ...(backend.private && backend.status !== "locked"
      ? [
          {
            label: "Lock",
            icon: <LockIcon />,
            onClick: () => lock.mutate({ path: { id: backend.id } }),
          },
        ]
      : []),
    { label: "Edit", icon: <PencilIcon />, onClick: () => setEditing(backend) },
    {
      label: "Test connection",
      icon: <FlaskConicalIcon />,
      disabled: test.isPending || backend.status === "locked",
      onClick: () => test.mutate({ path: { id: backend.id } }),
    },
    {
      label: "Remove",
      icon: <Trash2Icon />,
      destructive: true,
      onClick: () => setRemoving(backend),
    },
  ];
  return (
    <SettingsCard
      title="Your storage"
      description="Drives you make here are yours to look after: the server doesn't back them up."
      action={
        <>
          {list.length > 0 && (
            <Button size="sm" variant="outline" onClick={() => setNewDrive(true)}>
              New drive
            </Button>
          )}
          {allowed && (
            <Button size="sm" variant="outline" onClick={() => setEditing(null)}>
              <PlusIcon />
              Add S3 storage
            </Button>
          )}
        </>
      }
      error={backends.error}
    >
      {list.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No storage of your own yet. Add an S3 bucket, then make drives on it and share them.
        </p>
      ) : (
        <div className="flex flex-col divide-y">
          {list.map((backend) => (
            <BackendRow
              key={backend.id}
              backend={backend}
              menu={<RowMenu label={`Actions for ${backend.name}`} actions={actions(backend)} />}
            />
          ))}
        </div>
      )}
      {list.some((b) => b.private && b.status === "locked") && !unlocked && (
        <p className="mt-2 text-sm text-muted-foreground">
          Unlock encryption in Settings → Encryption to unlock your private storage.
        </p>
      )}
      {!allowed && list.length > 0 && (
        <p className="mt-2 text-sm text-muted-foreground">
          Your administrator no longer allows adding storage of your own.
        </p>
      )}
      {editing !== undefined && (
        <BackendDialog
          backend={editing ?? undefined}
          own
          me={user.id}
          onClose={() => setEditing(undefined)}
        />
      )}
      {newDrive && <NewDriveDialog onClose={() => setNewDrive(false)} />}
      {removing && (
        <ConfirmDialog
          title={`Remove ${removing.name}?`}
          description={
            removing.drives > 0
              ? `Its ${removing.drives === 1 ? "drive goes" : `${removing.drives} drives go`} out of Organizer for everyone in them. The files stay in your bucket.`
              : "Organizer forgets this storage. Nothing in the bucket is deleted."
          }
          action="Remove"
          confirmText={removing.drives > 0 ? removing.name : undefined}
          onConfirm={() => remove.mutate({ path: { id: removing.id } })}
          onClose={() => setRemoving(undefined)}
        />
      )}
    </SettingsCard>
  );
}
