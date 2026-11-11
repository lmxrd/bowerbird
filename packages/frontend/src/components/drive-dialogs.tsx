import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { toast } from "sonner";
import type { Drive } from "organizer-sdk";
import {
  backendChoicesOptions,
  createDriveMutation,
  renameDriveMutation,
  updateSharedDriveMutation,
} from "organizer-sdk/react-query";
import { StoredOnField } from "@/components/backends";
import { FormDialog } from "@/components/form-dialog";
import { RenameDialog } from "@/components/name-dialogs";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { queryClient } from "@/lib/api";
import { DISK_ID, chosenBackend } from "@/lib/backends";

/** The drive's lists, fetched again after its name changed. */
function refreshDrives() {
  for (const _id of ["listDrives", "getDrive", "listSharedDrives"]) {
    void queryClient.invalidateQueries({ queryKey: [{ _id }] });
  }
}

/** Rename a drive the user manages. */
export function RenameDriveDialog({ drive, onClose }: { drive: Drive; onClose: () => void }) {
  const rename = useMutation({
    ...renameDriveMutation(),
    onSuccess: () => {
      refreshDrives();
      onClose();
    },
  });
  return (
    <RenameDialog
      initial={drive.name}
      pending={rename.isPending}
      error={rename.error}
      onSave={(name) => rename.mutate({ path: { id: drive.id }, body: { name } })}
      onClose={onClose}
    />
  );
}

/** Rename a shared drive as the superadmin. */
export function RenameSharedDriveDialog({ drive, onClose }: { drive: Drive; onClose: () => void }) {
  const rename = useMutation({
    ...updateSharedDriveMutation(),
    onSuccess: () => {
      refreshDrives();
      onClose();
    },
  });
  return (
    <RenameDialog
      initial={drive.name}
      pending={rename.isPending}
      error={rename.error}
      onSave={(name) => rename.mutate({ path: { id: drive.id }, body: { name } })}
      onClose={onClose}
    />
  );
}

/**
 * "New drive" of a user (`POST /drives`): its name and where it is stored,
 * of the places the user may use (`/backends/choices`), their own storage
 * marked. They manage it and add its members from its page.
 */
export function NewDriveDialog({ onClose }: { onClose: () => void }) {
  const choices = useQuery(backendChoicesOptions());
  const [name, setName] = useState("");
  // Their own storage is chosen at first: this opens from it.
  const [storedOn, setStoredOn] = useState("");
  const shown = storedOn || choices.data?.find((c) => c.owner)?.id || DISK_ID;
  const create = useMutation({
    ...createDriveMutation(),
    onSuccess: (drive) => {
      refreshDrives();
      toast.success(`${drive.name} is made`);
      onClose();
    },
  });
  return (
    <FormDialog
      title="New drive"
      description="You manage it: add its members from its page."
      action="Create drive"
      pending={create.isPending}
      error={create.error}
      onSubmit={() =>
        create.mutate({ body: { name: name.trim(), backend_id: chosenBackend(shown) ?? null } })
      }
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="new-drive-name">Name</FieldLabel>
        <Input
          id="new-drive-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
          required
        />
      </Field>
      <StoredOnField choices={choices.data} value={shown} onChange={setStoredOn} />
    </FormDialog>
  );
}
