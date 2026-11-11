import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { toast } from "sonner";
import type { Drive, Mirror, MirrorSchedule, MirrorStatus, MirrorStopped } from "organizer-sdk";
import {
  createMirrorMutation,
  deleteMirrorMutation,
  ejectMirrorMutation,
  listBackendsOptions,
  listMirrorsOptions,
  runMirrorMutation,
  updateMirrorMutation,
} from "organizer-sdk/react-query";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { FormDialog } from "@/components/form-dialog";
import { SelectField } from "@/components/settings";
import { Badge } from "@/components/ui/badge";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { errorMessage, queryClient } from "@/lib/api";
import { drivesOptions } from "@/lib/drive-files";
import { driveUrl } from "@/lib/drives";
import {
  SCHEDULES,
  STATUS_LABELS,
  ejectName,
  nameTaken,
  refusalText,
  statusVariant,
  stoppedText,
} from "@/lib/mirrors";

/**
 * Mirrors' dialogs and badge (docs/M7D_DESIGN.md §7): "Mirror…" from a
 * drive's page, Change, Eject… and Remove… from Settings → Mirrors and a
 * mirror's page.
 */

/** Every query a mirror shows in: its lists, its drive, the drive's access list. */
export function refreshMirrors() {
  for (const _id of [
    "listMirrors",
    "getMirror",
    "listDrives",
    "getDrive",
    "getDriveAccess",
    "listTrash",
  ]) {
    void queryClient.invalidateQueries({ queryKey: [{ _id }] });
  }
}

/** A refusal in words: by its `code` when the mirrors' endpoints give one. */
export function mirrorError(error: unknown): string | undefined {
  if (!error) return undefined;
  return refusalText(error) ?? errorMessage(error);
}

/**
 * The user's mirrors, asked again every few seconds while one copies or
 * waits to (its progress and status come from the server's loop).
 */
export function useMirrors() {
  return useQuery({
    ...listMirrorsOptions(),
    refetchInterval: (query) =>
      query.state.data?.some((m) => m.status === "copying" || m.status === "pending")
        ? 3000
        : false,
  });
}

/** A mirror's status as a badge: "Up to date", "Stopped" (with why in its title). */
export function MirrorStatusBadge({
  status,
  stopped,
}: {
  status: MirrorStatus;
  stopped?: MirrorStopped | null;
}) {
  return (
    <Badge variant={statusVariant(status)} title={stopped ? stoppedText(stopped) : undefined}>
      {STATUS_LABELS[status]}
    </Badge>
  );
}

/**
 * "Mirror…": to which of the user's own storage, and when. The server
 * says whether the drive may be mirrored; a refusal shows by its code.
 */
export function MirrorDialog({ drive, onClose }: { drive: Drive; onClose: () => void }) {
  const navigate = useNavigate();
  const backends = useQuery(listBackendsOptions());
  const [to, setTo] = useState("");
  const [schedule, setSchedule] = useState<MirrorSchedule>("continuous");
  const [problem, setProblem] = useState<string>();
  const backend = to || backends.data?.[0]?.id || "";
  const create = useMutation({
    ...createMirrorMutation(),
    onSuccess: (mirror) => {
      refreshMirrors();
      toast.success(`Mirroring ${drive.name}`);
      onClose();
      void navigate({ to: driveUrl({ id: mirror.id, kind: "mirror" }, "/") });
    },
  });
  return (
    <FormDialog
      title={`Mirror "${drive.name}"`}
      description="A read-only copy in your own storage, kept up to date. Deleted and changed files stay in the mirror's trash and versions."
      action={create.isPending ? "Starting…" : "Start mirroring"}
      pending={create.isPending}
      error={problem ?? mirrorError(create.error)}
      onSubmit={() => {
        setProblem(backend ? undefined : "Add storage of your own first, in Settings");
        if (backend) {
          create.mutate({ body: { drive_id: drive.id, backend_id: backend, schedule } });
        }
      }}
      onClose={onClose}
    >
      <SelectField
        id="mirror-to"
        label="To"
        placeholder="Your storage"
        items={(backends.data ?? []).map((b) => ({
          value: b.id,
          label: b.status === "ok" ? b.name : `${b.name} (${b.status})`,
        }))}
        value={backend}
        onChange={setTo}
      />
      <SelectField
        id="mirror-when"
        label="When"
        items={SCHEDULES}
        value={schedule}
        onChange={(value) => setSchedule(value as MirrorSchedule)}
      />
      <p className="text-sm text-muted-foreground">
        {drive.kind === "shared"
          ? "The drive's members will see that you mirror it. Only what you can see is copied."
          : "Only what you can see is copied: shares placed in My drive are not."}
      </p>
    </FormDialog>
  );
}

/** "Change": the mirror's schedule. */
export function ScheduleDialog({ mirror, onClose }: { mirror: Mirror; onClose: () => void }) {
  const [schedule, setSchedule] = useState<MirrorSchedule>(mirror.schedule);
  const update = useMutation({
    ...updateMirrorMutation(),
    onSuccess: () => {
      refreshMirrors();
      onClose();
    },
  });
  return (
    <FormDialog
      title={`Mirror of ${mirror.drive_name}`}
      description="When it copies the drive's changes."
      action="Save"
      pending={update.isPending}
      error={mirrorError(update.error)}
      onSubmit={() => update.mutate({ path: { id: mirror.id }, body: { schedule } })}
      onClose={onClose}
    >
      <SelectField
        id="mirror-schedule"
        label="When"
        items={SCHEDULES}
        value={schedule}
        onChange={(value) => setSchedule(value as MirrorSchedule)}
      />
    </FormDialog>
  );
}

/** "Mirror now", with a toast either way (a stopped mirror is refused by its code). */
export function useRunMirror() {
  return useMutation({
    ...runMirrorMutation(),
    onSuccess: () => {
      refreshMirrors();
      toast.success("Copying the drive's changes");
    },
    onError: (error) => toast.error(mirrorError(error)),
  });
}

/**
 * "Eject…": make the mirror a drive where it is (mirroring stops, nothing
 * is copied), or a new drive with what it holds now (it goes on). A name
 * when its own is taken among the drives the user knows, or the server
 * refused it.
 */
export function EjectDialog({
  mirror,
  onClose,
}: {
  mirror: Pick<Mirror, "id" | "drive_name">;
  onClose: () => void;
}) {
  const navigate = useNavigate();
  const drives = useQuery(drivesOptions());
  const [mode, setMode] = useState<"in_place" | "copy">("in_place");
  const taken = (drives.data ?? [])
    .filter((d) => d.kind === "shared" || (mode === "copy" && d.id === mirror.id))
    .map((d) => d.name);
  const suggested = ejectName(mirror.drive_name, taken);
  const [name, setName] = useState<string>();
  const eject = useMutation({
    ...ejectMirrorMutation(),
    onSuccess: (drive) => {
      refreshMirrors();
      toast.success(mode === "in_place" ? `${drive.name} is a drive now` : `${drive.name} is made`);
      onClose();
      void navigate({ to: driveUrl(drive, "/") });
    },
  });
  // A name is asked when its own is taken among the drives the user knows,
  // or the server says it is (`name_taken`: one they do not know).
  const askName = suggested.needed || name !== undefined || nameTaken(eject.error);
  return (
    <FormDialog
      title={`Eject ${mirror.drive_name}`}
      action={
        eject.isPending ? "Working…" : mode === "in_place" ? "Make it a drive" : "Make a copy"
      }
      pending={eject.isPending}
      error={mirrorError(eject.error)}
      onSubmit={() =>
        eject.mutate({
          path: { id: mirror.id },
          body: { mode, ...(askName ? { name: (name ?? suggested.name).trim() } : {}) },
        })
      }
      onClose={onClose}
    >
      <RadioGroup value={mode} onValueChange={(value) => setMode(value as typeof mode)}>
        <Label className="items-start font-normal">
          <RadioGroupItem value="in_place" />
          <span className="flex flex-col gap-1">
            <span className="font-medium">Make it a drive</span>
            <span className="text-muted-foreground">
              It becomes a drive where it is, and you manage it. Mirroring stops. Nothing is copied.
            </span>
          </span>
        </Label>
        <Label className="items-start font-normal">
          <RadioGroupItem value="copy" />
          <span className="flex flex-col gap-1">
            <span className="font-medium">Make a copy</span>
            <span className="text-muted-foreground">
              A new drive with what the mirror holds now. The mirror goes on mirroring.
            </span>
          </span>
        </Label>
      </RadioGroup>
      {askName && (
        <Field>
          <FieldLabel htmlFor="eject-name">Name</FieldLabel>
          <Input
            id="eject-name"
            value={name ?? suggested.name}
            onChange={(e) => setName(e.target.value)}
            required
          />
          <FieldDescription>Each shared drive has a name of its own.</FieldDescription>
        </Field>
      )}
    </FormDialog>
  );
}

/** "Remove…": the copy is deleted, so the confirmation names the bucket it is in. */
export function RemoveMirrorDialog({
  mirror,
  onRemoved,
  onClose,
}: {
  mirror: Pick<Mirror, "id" | "drive_name" | "bucket" | "location">;
  onRemoved?: () => void;
  onClose: () => void;
}) {
  const remove = useMutation({
    ...deleteMirrorMutation(),
    onSuccess: () => {
      refreshMirrors();
      toast.success(`The mirror of ${mirror.drive_name} is removed`);
      onRemoved?.();
    },
    onError: (error) => toast.error(mirrorError(error)),
  });
  return (
    <ConfirmDialog
      title={`Remove the mirror of ${mirror.drive_name}?`}
      description={
        <>
          Its copy is deleted from the bucket <span className="font-mono">{mirror.bucket}</span> (
          <span className="font-mono">{mirror.location}</span>), with its trash and versions. The
          drive itself is not touched. This can't be undone.
        </>
      }
      action="Remove"
      onConfirm={() => remove.mutate({ path: { id: mirror.id } })}
      onClose={onClose}
    />
  );
}
