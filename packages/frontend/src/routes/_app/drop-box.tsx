import { useEffect, useRef, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { FolderUpIcon, LinkIcon, UploadIcon } from "lucide-react";
import { toast } from "sonner";
import type { DroppedFolders, Links } from "organizer-sdk/types";
import { dropLinkMutation, getDropBoxSettingsOptions } from "organizer-sdk/react-query";
import { DropBoxReview } from "@/components/drop-box-review";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { errorMessage, queryClient } from "@/lib/api";
import { type Dropped, collectEntries, fromFile, tooMany, topFolders } from "@/lib/folder-drop";
import { dropFiles } from "@/lib/transfer";

export const Route = createFileRoute("/_app/drop-box")({ component: DropBox });

const linkModes: { value: Links; label: string }[] = [
  { value: "bookmark", label: "Bookmark" },
  { value: "snapshot", label: "Bookmark and page snapshot" },
  { value: "download", label: "Download the file" },
];

const refresh = () => queryClient.invalidateQueries();

function DropBox() {
  const settings = useQuery(getDropBoxSettingsOptions());
  const [note, setNote] = useState("");
  const [readContents, setReadContents] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [uploading, setUploading] = useState(0);
  const [link, setLink] = useState<string>();
  const input = useRef<HTMLInputElement>(null);
  const folderInput = useRef<HTMLInputElement>(null);
  // A drop with folders waits for the choice of keeping them whole or not.
  const [held, setHeld] = useState<Dropped[]>();
  const [folders, setFolders] = useState<DroppedFolders>("whole");

  // Per item only where the settings leave it to each item.
  const perItem = settings.data?.read_contents !== "always";
  useEffect(() => {
    if (settings.data) setReadContents(settings.data.read_contents === "default_on");
  }, [settings.data]);
  const choice = () => ({
    note: note.trim() || undefined,
    readContents: settings.data && perItem ? readContents : undefined,
  });

  const send = async (files: Dropped[], folders?: DroppedFolders) => {
    setUploading((n) => n + files.length);
    try {
      await dropFiles(files, { ...choice(), folders });
      setNote("");
      const top = topFolders(files);
      toast.success(
        files.length === 1
          ? `Dropped ${files[0]?.path}`
          : top.length === 1 && folders === "whole"
            ? `Dropped ${top[0]}`
            : `Dropped ${files.length} files`,
      );
    } catch (error) {
      toast.error(errorMessage(error));
    } finally {
      setUploading((n) => n - files.length);
      void refresh();
    }
  };
  const drop = (files: Dropped[]) => {
    if (files.length === 0) return;
    const refused = tooMany(files.length);
    if (refused) {
      toast.error(refused);
      return;
    }
    if (topFolders(files).length === 0) {
      void send(files);
      return;
    }
    setFolders(settings.data?.folders ?? "whole");
    setHeld(files);
  };
  const heldFolders = held ? topFolders(held) : [];

  return (
    <>
      <PageHeader crumbs={[{ label: "Drop box" }]} />
      <div className="flex flex-1 flex-col gap-4 p-4">
        <h1 className="text-lg font-medium">Drop box</h1>
        <div
          tabIndex={0}
          className={`flex flex-col items-center gap-3 rounded-lg border border-dashed p-6 text-center outline-none ${dragging ? "bg-muted" : ""}`}
          onDragOver={(e) => {
            e.preventDefault();
            setDragging(true);
          }}
          onDragLeave={() => setDragging(false)}
          onDrop={(e) => {
            e.preventDefault();
            setDragging(false);
            // The entries must be taken during the event; reading them is
            // async. A dragged folder is read all the way down.
            const entries = Array.from(e.dataTransfer.items)
              .filter((item) => item.kind === "file")
              .map((item) => item.webkitGetAsEntry?.())
              .filter((entry): entry is FileSystemEntry => !!entry);
            if (entries.length) {
              collectEntries(entries).then(drop, (error: unknown) =>
                toast.error(errorMessage(error, "The folder could not be read")),
              );
              return;
            }
            if (e.dataTransfer.files.length) {
              drop(Array.from(e.dataTransfer.files, fromFile));
              return;
            }
            const url = e.dataTransfer.getData("text/uri-list") || e.dataTransfer.getData("text");
            if (/^https?:\/\//i.test(url.trim())) setLink(url.trim());
          }}
          onPaste={(e) => {
            // A link pasted on the zone (not into the note) opens the link dialog.
            if (e.target instanceof HTMLInputElement) return;
            if (e.clipboardData.files.length) {
              e.preventDefault();
              drop(Array.from(e.clipboardData.files, fromFile));
              return;
            }
            const url = e.clipboardData.getData("text").trim();
            if (/^https?:\/\//i.test(url)) {
              e.preventDefault();
              setLink(url);
            }
          }}
        >
          <UploadIcon className="size-6 text-muted-foreground" />
          <p className="font-medium">Drop files or folders here, or paste a link</p>
          <Input
            className="max-w-md"
            placeholder="A note for the AI (optional)"
            aria-label="A note for the AI"
            maxLength={2000}
            value={note}
            onChange={(e) => setNote(e.target.value)}
          />
          {settings.data && perItem && (
            <div className="flex items-center gap-2">
              <Switch
                id="read-contents"
                checked={readContents}
                onCheckedChange={(checked) => setReadContents(checked)}
              />
              <Label htmlFor="read-contents">Read contents</Label>
            </div>
          )}
          {held && (
            <div className="flex flex-col items-center gap-3 rounded-lg border p-3">
              <p className="text-sm">
                {heldFolders.join(", ")}:{" "}
                {held.length === 1 ? "1 file" : `${held.length.toLocaleString()} files`}
              </p>
              <RadioGroup
                aria-label="Dropped folders"
                className="flex w-auto flex-wrap justify-center gap-4"
                value={folders}
                onValueChange={(value) => setFolders(value as DroppedFolders)}
              >
                <Label className="font-normal">
                  <RadioGroupItem value="whole" />
                  Keep folders whole
                </Label>
                <Label className="font-normal">
                  <RadioGroupItem value="split" />
                  Split into files
                </Label>
              </RadioGroup>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  onClick={() => {
                    void send(held, folders);
                    setHeld(undefined);
                  }}
                >
                  Drop
                </Button>
                <Button size="sm" variant="outline" onClick={() => setHeld(undefined)}>
                  Cancel
                </Button>
              </div>
            </div>
          )}
          <div className="flex flex-wrap justify-center gap-2">
            <Button onClick={() => input.current?.click()} disabled={uploading > 0}>
              <UploadIcon />
              {uploading > 0 ? `Dropping ${uploading}…` : "Choose files"}
            </Button>
            <Button
              variant="outline"
              onClick={() => folderInput.current?.click()}
              disabled={uploading > 0}
            >
              <FolderUpIcon />
              Choose folder
            </Button>
            <Button variant="outline" onClick={() => setLink("")}>
              <LinkIcon />
              Add link
            </Button>
          </div>
          <input
            ref={input}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              if (e.target.files?.length) drop(Array.from(e.target.files, fromFile));
              e.target.value = "";
            }}
          />
          <input
            ref={(element) => {
              folderInput.current = element;
              // Not in React's types: the input chooses a folder, and gives
              // its files with their path in it (webkitRelativePath).
              element?.setAttribute("webkitdirectory", "");
            }}
            type="file"
            hidden
            onChange={(e) => {
              if (e.target.files?.length) drop(Array.from(e.target.files, fromFile));
              e.target.value = "";
            }}
          />
        </div>

        <DropBoxReview />
      </div>

      {link !== undefined && (
        <LinkDialog
          initial={link}
          mode={settings.data?.links ?? "bookmark"}
          options={choice()}
          onDone={() => {
            setLink(undefined);
            setNote("");
          }}
          onClose={() => setLink(undefined)}
        />
      )}
    </>
  );
}

/** Drop a link: its address, and what to keep of it. */
function LinkDialog({
  initial,
  mode: defaultMode,
  options,
  onDone,
  onClose,
}: {
  initial: string;
  mode: Links;
  options: { note?: string; readContents?: boolean };
  onDone: () => void;
  onClose: () => void;
}) {
  const [url, setUrl] = useState(initial);
  const [mode, setMode] = useState<Links>(defaultMode);
  const add = useMutation({
    ...dropLinkMutation(),
    onSuccess: () => {
      toast.success("Link dropped");
      void refresh();
      onDone();
    },
  });
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            add.mutate({
              body: {
                url: url.trim(),
                mode,
                note: options.note ?? null,
                read_contents: options.readContents ?? null,
              },
            });
          }}
        >
          <DialogHeader>
            <DialogTitle>Add link</DialogTitle>
          </DialogHeader>
          <FieldGroup>
            <Field>
              <FieldLabel htmlFor="url">Address</FieldLabel>
              <Input
                id="url"
                type="url"
                placeholder="https://"
                value={url}
                required
                autoFocus
                onChange={(e) => setUrl(e.target.value)}
              />
            </Field>
            <Field>
              <FieldLabel>Keep</FieldLabel>
              <Select
                items={linkModes}
                value={mode}
                onValueChange={(value) => value && setMode(value)}
              >
                <SelectTrigger className="w-full">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {linkModes.map((m) => (
                    <SelectItem key={m.value} value={m.value}>
                      {m.label}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              {add.isError && <FieldError>{errorMessage(add.error)}</FieldError>}
            </Field>
          </FieldGroup>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={add.isPending}>
              Add link
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
