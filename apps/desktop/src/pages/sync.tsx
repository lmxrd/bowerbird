import { useMutation } from "@tanstack/react-query";
import {
  AlertTriangleIcon,
  ArrowDownIcon,
  ArrowRightIcon,
  ArrowUpIcon,
  CheckCircle2Icon,
  CloudOffIcon,
  CloudAlertIcon,
  CopyIcon,
  EyeIcon,
  FolderIcon,
  FolderXIcon,
  InboxIcon,
  LockIcon,
  PauseIcon,
  PlayIcon,
  RefreshCwIcon,
  Trash2Icon,
  Undo2Icon,
} from "lucide-react";
import { toast } from "sonner";
import { EmptyState } from "@/components/empty-state";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Item,
  ItemContent,
  ItemDescription,
  ItemGroup,
  ItemMedia,
  ItemSeparator,
  ItemTitle,
} from "@/components/ui/item";
import { Progress } from "@/components/ui/progress";
import { errorMessage } from "@/lib/api";
import { formatDate } from "@/lib/format";
import { Page } from "../components/page";
import { type Activity, type ActivityKind, type View, commands } from "../lib/desktop";
import { unavailableNote } from "../lib/drives";
import {
  activityLabel,
  encryptionNote,
  folderCount,
  headline,
  heldReason,
  host,
  notSyncedTitle,
  progress,
  waitingNote,
} from "../lib/status";
import { Fragment } from "react";

const icons: Record<ActivityKind, React.ReactNode> = {
  upload: <ArrowUpIcon />,
  download: <ArrowDownIcon />,
  move: <ArrowRightIcon />,
  delete: <Trash2Icon />,
  conflict: <CopyIcon />,
  drop_box: <InboxIcon />,
  restore: <Undo2Icon />,
};

/** The sync status: the headline with its progress and Pause, the conflicts, recent activity. */
export function SyncPage({ view, onSettings }: { view: View; onSettings: () => void }) {
  const { status, paused } = view;
  const toggle = useMutation({
    mutationFn: () => (paused ? commands.resume() : commands.pause()),
    onError: (error) => toast.error(errorMessage(error)),
  });
  const done = progress(status);
  const line = headline(status, paused);
  const locked = encryptionNote(status.encryption);
  const gone = unavailableNote(view.folders);
  const held = notSyncedTitle(status.not_synced);
  const waiting = waitingNote(status.waiting);
  const icon =
    paused || status.phase === "paused" ? (
      <PauseIcon />
    ) : status.phase === "offline" ? (
      <CloudOffIcon />
    ) : status.phase === "error" ? (
      <AlertTriangleIcon className="text-destructive" />
    ) : done !== null ? (
      <RefreshCwIcon className="animate-spin" />
    ) : (
      <CheckCircle2Icon />
    );
  const detail = [
    host(view.server),
    folderCount(view.folders.length),
    status.synced_at ? `last synced ${formatDate(status.synced_at)}` : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <Page
      title="Sync"
      action={
        <Button variant="outline" disabled={toggle.isPending} onClick={() => toggle.mutate()}>
          {paused ? <PlayIcon /> : <PauseIcon />}
          {paused ? "Resume" : "Pause"}
        </Button>
      }
    >
      <div className="flex flex-col gap-3 rounded-lg border p-4">
        <div className="flex items-center gap-3 [&>svg]:size-6 [&>svg]:shrink-0">
          {icon}
          <div className="min-w-0">
            <p className="truncate text-base font-medium">
              {status.phase === "error" ? "Something went wrong" : line}
            </p>
            <p className="truncate text-sm text-muted-foreground">
              {status.phase === "error" && status.error ? status.error : detail}
            </p>
          </div>
        </div>
        {done !== null && <Progress value={done} aria-label="Sync progress" />}
      </div>

      {locked && (
        <Alert>
          <LockIcon />
          <AlertTitle>Encrypted folders wait</AlertTitle>
          <AlertDescription>
            <p>{locked} Nothing in them is changed or deleted meanwhile, here or on the server.</p>
            {!status.encryption.unlocked && (
              <Button variant="link" className="h-auto p-0" onClick={onSettings}>
                Unlock in the settings
              </Button>
            )}
          </AlertDescription>
        </Alert>
      )}

      {gone && (
        <Alert>
          <FolderXIcon />
          <AlertTitle>Folders not synced</AlertTitle>
          <AlertDescription>
            <p>{gone} Their files stay on this computer.</p>
            <Button variant="link" className="h-auto p-0" onClick={onSettings}>
              See them in the settings
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {waiting && (
        <Alert>
          <CloudAlertIcon />
          <AlertTitle>Folders wait for their storage</AlertTitle>
          <AlertDescription>
            <p>{waiting} Nothing is deleted meanwhile, here or on the server.</p>
            <Button variant="link" className="h-auto p-0" onClick={onSettings}>
              See them in the settings
            </Button>
          </AlertDescription>
        </Alert>
      )}

      {held && (
        <Alert>
          <EyeIcon />
          <AlertTitle>{held}</AlertTitle>
          <AlertDescription>
            <p>
              Your role on these does not let them be sent, they are no longer shared with you, or
              their drive's storage is not available now. They stay on this computer as they are:
            </p>
            <ul className="list-disc pl-4">
              {status.not_synced.slice(0, 5).map((item) => (
                <li key={item.path}>
                  {item.path} — {heldReason(item)}
                </li>
              ))}
            </ul>
            {status.not_synced.length > 5 && <p>And {status.not_synced.length - 5} more.</p>}
          </AlertDescription>
        </Alert>
      )}

      {status.conflicts.length > 0 && (
        <Alert>
          <CopyIcon />
          <AlertTitle>
            {status.conflicts.length === 1
              ? "1 conflict: both versions kept"
              : `${status.conflicts.length} conflicts: both versions kept`}
          </AlertTitle>
          <AlertDescription>
            <p>
              These changed here and on the server. The server's version is in place; yours is
              beside it:
            </p>
            <ul className="list-disc pl-4">
              {status.conflicts.slice(0, 5).map((c) => (
                <li key={c.copy + c.at}>{c.copy}</li>
              ))}
            </ul>
          </AlertDescription>
        </Alert>
      )}

      <h2 className="text-sm font-medium">Recent activity</h2>
      {view.folders.length === 0 ? (
        <EmptyState icon={FolderIcon} title="No folders synced yet" className="flex-none">
          <Button variant="link" className="h-auto p-0" onClick={onSettings}>
            Choose a drive or a folder
          </Button>{" "}
          in the settings to keep it on this computer.
        </EmptyState>
      ) : status.recent.length === 0 ? (
        <EmptyState icon={RefreshCwIcon} title="Nothing synced yet" className="flex-none">
          Uploads, downloads and moves show here as they happen.
        </EmptyState>
      ) : (
        <ItemGroup className="gap-0 rounded-lg border">
          {status.recent.map((activity, i) => (
            <Fragment key={`${activity.at}-${i}`}>
              {i > 0 && <ItemSeparator className="my-0" />}
              <ActivityRow activity={activity} />
            </Fragment>
          ))}
        </ItemGroup>
      )}
    </Page>
  );
}

function ActivityRow({ activity }: { activity: Activity }) {
  return (
    <Item size="sm">
      <ItemMedia variant="icon" className="text-muted-foreground">
        {icons[activity.kind]}
      </ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle className="w-full truncate">{activity.path}</ItemTitle>
        <ItemDescription>{activityLabel(activity.kind)}</ItemDescription>
      </ItemContent>
      <span className="text-sm text-muted-foreground tabular-nums">{formatDate(activity.at)}</span>
    </Item>
  );
}
