import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import type { Drive } from "organizer-sdk/types";
import { type BrowsedFolder, FolderBrowser } from "@/components/place-dialog";
import { FormDialog } from "@/components/form-dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Textarea } from "@/components/ui/textarea";
import { type FolderMode, type OnDemand, type SyncedFolder, commands } from "../lib/desktop";
import { defaultPlace, driveLabel, remoteOrigin } from "../lib/drives";
import { parsePatterns } from "../lib/status";

/**
 * Sync a drive, or a folder in it: which (all of the drive, or a folder
 * browsed to), how it is kept (on macOS and Windows, on demand: its files
 * download when opened), and where it goes on this computer (by default
 * `~/Organizer/<drive or folder name>`; on macOS an on-demand folder is in
 * Finder's Locations instead).
 */
export function SyncDriveDialog({
  drive,
  organizerDir,
  taken,
  onDemand,
  onClose,
}: {
  drive: Drive;
  organizerDir: string;
  /** The synced folders' places and the drop box folder's. */
  taken: string[];
  onDemand: OnDemand;
  onClose: () => void;
}) {
  // The folders from the drive's root down to the one shown; empty at it.
  const [trail, setTrail] = useState<BrowsedFolder[]>([]);
  const [local, setLocal] = useState("");
  const [chosen, setMode] = useState<FolderMode>("sync");
  // An encrypted folder syncs in full: a placeholder would only have the
  // server's sealed bytes (the engine refuses it too).
  const encrypted = trail.some((f) => f.encrypted);
  const mode: FolderMode = encrypted ? "sync" : chosen;
  // In Finder's Locations, where the system puts it.
  const inFinder = mode === "on_demand" && onDemand === "file_provider";
  // The folder shown is the one synced; at the drive's root, all of it.
  const folder = trail.at(-1);
  // A mirror is named as one ("Team · mirror"), apart from the drive it copies.
  const label = driveLabel(drive);
  const name = folder?.name ?? label;
  const mirror = drive.kind === "mirror";
  const add = useMutation({
    mutationFn: async () => {
      const id = folder?.id ?? drive.root_id;
      if (!id) throw new Error(`${label} is not available on the server now`);
      await commands.addFolder(id, name, (!inFinder && local.trim()) || undefined, mode);
    },
    onSuccess: onClose,
  });
  // The browser starts at My drive's root: another drive's root (a shared
  // drive's, a mirror's, one from another server) is put first in the way down, and going back above
  // it stays at it.
  const top: BrowsedFolder[] =
    drive.kind !== "my" && drive.root_id ? [{ id: drive.root_id, name: label }] : [];
  const browse = (next: BrowsedFolder[]) =>
    setTrail(top.length === 0 ? next : next[0]?.id === top[0].id ? next.slice(1) : []);
  return (
    <FormDialog
      title={`Sync ${label}`}
      description={
        mirror
          ? "A read-only copy: what the mirror holds is kept here; nothing changed here is sent."
          : drive.kind === "remote"
            ? `${remoteOrigin(drive) ?? "From another server"}, through your server. It is kept the same here and there, as your role allows.`
            : "It is kept the same here and on the server, both ways."
      }
      action="Sync"
      pending={add.isPending}
      error={add.error}
      onSubmit={() => add.mutate()}
      onClose={onClose}
    >
      <Field>
        <FieldLabel>On the server</FieldLabel>
        <FolderBrowser trail={[...top, ...trail]} onTrail={browse} />
        <FieldDescription>
          {folder ? `${name} is synced.` : `All of ${label} is synced, or open a folder.`}
        </FieldDescription>
      </Field>
      {onDemand && (
        <Field>
          <FieldLabel>Files</FieldLabel>
          <RadioGroup value={mode} onValueChange={(value) => setMode(value as FolderMode)}>
            <Label className="font-normal">
              <RadioGroupItem value="sync" />
              Sync: every file is kept on this computer
            </Label>
            <Label className="font-normal">
              <RadioGroupItem value="on_demand" disabled={encrypted} />
              On-demand: files show, and download when opened
            </Label>
          </RadioGroup>
          {encrypted && (
            <FieldDescription>
              An encrypted folder is synced in full, never on demand.
            </FieldDescription>
          )}
        </Field>
      )}
      {inFinder ? (
        <Field>
          <FieldLabel>On this computer</FieldLabel>
          <FieldDescription>In Finder, under Locations.</FieldDescription>
        </Field>
      ) : (
        <Field>
          <FieldLabel htmlFor="local-place">On this computer</FieldLabel>
          <Input
            id="local-place"
            placeholder={defaultPlace(organizerDir, name, taken)}
            value={local}
            onChange={(e) => setLocal(e.target.value)}
          />
          <FieldDescription>A full path; empty for the one shown.</FieldDescription>
        </Field>
      )}
    </FormDialog>
  );
}

/** Move a synced folder's local place: its files move there. */
export function LocalPlaceDialog({
  folder,
  onClose,
}: {
  folder: SyncedFolder;
  onClose: () => void;
}) {
  const [local, setLocal] = useState(folder.local_path);
  const move = useMutation({
    mutationFn: () => commands.moveFolder(folder.id, local),
    onSuccess: onClose,
  });
  return (
    <FormDialog
      title={`Change where ${folder.name} is`}
      description="Its files are moved there."
      action="Move"
      pending={move.isPending}
      error={move.error}
      onSubmit={() => move.mutate()}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="new-place">On this computer</FieldLabel>
        <Input
          id="new-place"
          required
          autoFocus
          value={local}
          onChange={(e) => setLocal(e.target.value)}
        />
        <FieldDescription>A full path to a folder that does not exist yet.</FieldDescription>
      </Field>
    </FormDialog>
  );
}

/** Edit the ignored files: one name per line, `*` for any characters. */
export function IgnoreDialog({ patterns, onClose }: { patterns: string[]; onClose: () => void }) {
  const [text, setText] = useState(patterns.join("\n"));
  const save = useMutation({
    mutationFn: () => commands.setIgnore(parsePatterns(text)),
    onSuccess: onClose,
  });
  return (
    <FormDialog
      title="Ignored files"
      description="Files and folders of these names are never synced."
      action="Save"
      pending={save.isPending}
      error={save.error}
      onSubmit={() => save.mutate()}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="ignored">Names</FieldLabel>
        <Textarea
          id="ignored"
          rows={10}
          className="font-mono"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        <FieldDescription>One per line; * stands for any characters.</FieldDescription>
      </Field>
    </FormDialog>
  );
}
