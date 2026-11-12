import { useState } from "react";
import { Link, createFileRoute } from "@tanstack/react-router";
import {
  CalendarClockIcon,
  FolderSyncIcon,
  HardDriveUploadIcon,
  RefreshCwIcon,
  Trash2Icon,
} from "lucide-react";
import type { Mirror } from "organizer-sdk";
import { ListRow } from "@/components/list-row";
import {
  EjectDialog,
  MirrorStatusBadge,
  RemoveMirrorDialog,
  ScheduleDialog,
  useMirrors,
  useRunMirror,
} from "@/components/mirrors";
import { type RowAction, RowMenu } from "@/components/row-menu";
import { SettingsCard } from "@/components/settings";
import { driveUrl } from "@/lib/drives";
import { lastCopyText, scheduleLabel, stoppedText } from "@/lib/mirrors";
import { formatSize } from "@/lib/format";

/**
 * Settings → Mirrors (docs/M7D_DESIGN.md §5): each of the user's mirrors
 * with its drive, storage, schedule, last copy or progress and status, and
 * Mirror now, Change, Eject…, Remove….
 */
export const Route = createFileRoute("/_app/settings/mirrors")({ component: Mirrors });

type Dialog =
  | { kind: "schedule"; mirror: Mirror }
  | { kind: "eject"; mirror: Mirror }
  | { kind: "remove"; mirror: Mirror };

function Mirrors() {
  const mirrors = useMirrors();
  const run = useRunMirror();
  const [dialog, setDialog] = useState<Dialog>();
  const list = mirrors.data ?? [];
  const actions = (mirror: Mirror): RowAction[] => [
    {
      label: "Mirror now",
      icon: <RefreshCwIcon />,
      disabled: mirror.status === "stopped" || mirror.status === "copying" || run.isPending,
      onClick: () => run.mutate({ path: { id: mirror.id } }),
    },
    {
      label: "Change",
      icon: <CalendarClockIcon />,
      onClick: () => setDialog({ kind: "schedule", mirror }),
    },
    {
      label: "Eject…",
      icon: <HardDriveUploadIcon />,
      disabled: mirror.status === "copying",
      onClick: () => setDialog({ kind: "eject", mirror }),
    },
    {
      label: "Remove…",
      icon: <Trash2Icon />,
      destructive: true,
      onClick: () => setDialog({ kind: "remove", mirror }),
    },
  ];
  return (
    <SettingsCard
      title="Mirrors"
      description="Read-only copies of drives in your own storage, kept up to date. Start one with Mirror… on a drive's page."
      error={mirrors.error}
    >
      {list.length === 0 ? (
        <p className="text-sm text-muted-foreground">
          No mirrors yet. Open a drive and choose Mirror… to keep a copy of it in your own storage.
        </p>
      ) : (
        <div className="flex flex-col divide-y">
          {list.map((mirror) => (
            <div key={mirror.id} className="py-1">
              <ListRow
                icon={<FolderSyncIcon className="size-4 shrink-0 text-muted-foreground" />}
                title={
                  <span className="flex items-center gap-2">
                    <Link
                      to={driveUrl({ id: mirror.id, kind: "mirror" }, "/")}
                      className="truncate hover:underline"
                    >
                      {mirror.drive_name}
                    </Link>
                    <MirrorStatusBadge status={mirror.status} stopped={mirror.stopped} />
                  </span>
                }
                detail={[
                  mirror.backend.name,
                  scheduleLabel(mirror.schedule),
                  lastCopyText(mirror),
                  formatSize(mirror.used),
                ].join(" · ")}
              >
                <RowMenu label={`Actions for ${mirror.drive_name}`} actions={actions(mirror)} />
              </ListRow>
              {mirror.stopped && (
                <p className="pb-1 pl-6 text-sm text-destructive">{stoppedText(mirror.stopped)}</p>
              )}
              {mirror.status === "failing" && mirror.error && (
                <p className="pb-1 pl-6 text-sm text-destructive">{mirror.error}</p>
              )}
            </div>
          ))}
        </div>
      )}
      {dialog?.kind === "schedule" && (
        <ScheduleDialog mirror={dialog.mirror} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "eject" && (
        <EjectDialog mirror={dialog.mirror} onClose={() => setDialog(undefined)} />
      )}
      {dialog?.kind === "remove" && (
        <RemoveMirrorDialog mirror={dialog.mirror} onClose={() => setDialog(undefined)} />
      )}
    </SettingsCard>
  );
}
