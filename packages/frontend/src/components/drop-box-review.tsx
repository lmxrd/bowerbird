import { Fragment, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import {
  ArrowRightIcon,
  FileIcon,
  FolderIcon,
  InboxIcon,
  LinkIcon,
  LockIcon,
  MoreHorizontalIcon,
  RotateCcwIcon,
  Undo2Icon,
} from "lucide-react";
import { toast } from "sonner";
import type { ItemView, Status } from "organizer-sdk/types";
import {
  confirmDropBoxItemMutation,
  placeDropBoxItemMutation,
  rejectDropBoxItemMutation,
  retryDropBoxItemMutation,
  undoPlacementMutation,
} from "organizer-sdk/react-query";
import { EmptyState } from "@/components/empty-state";
import { PlaceDialog } from "@/components/place-dialog";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import {
  Item,
  ItemActions,
  ItemContent,
  ItemDescription,
  ItemGroup,
  ItemMedia,
  ItemSeparator,
  ItemTitle,
} from "@/components/ui/item";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { dropBoxItemsOptions, errorMessage, queryClient } from "@/lib/api";
import { ANALYSING, DONE, UNPLACED, WAITING, percent, statusLabel } from "@/lib/drop-box";

const tabs: { value: string; label: string; statuses?: readonly Status[] }[] = [
  { value: "waiting", label: "Waiting for you", statuses: WAITING },
  { value: "analysing", label: "Analysing", statuses: ANALYSING },
  { value: "done", label: "Done", statuses: DONE },
  { value: "all", label: "All" },
];

const refresh = () => queryClient.invalidateQueries();

/**
 * The drop box's review: its items by tab, each with what the AI proposes
 * and the actions on it (confirm, change, reject, place, retry, undo). The
 * web's drop box page and the desktop app's Drop box tab.
 */
export function DropBoxReview({ empty }: { empty?: string }) {
  const items = useQuery(dropBoxItemsOptions());
  const [tab, setTab] = useState("waiting");
  const [dialog, setDialog] = useState<{ kind: "change" | "place"; item: ItemView }>();

  const onError = (error: unknown) => toast.error(errorMessage(error));
  const confirm = useMutation({
    ...confirmDropBoxItemMutation(),
    onSuccess: () => {
      setDialog(undefined);
      void refresh();
    },
  });
  const place = useMutation({
    ...placeDropBoxItemMutation(),
    onSuccess: () => {
      setDialog(undefined);
      void refresh();
    },
  });
  const reject = useMutation({ ...rejectDropBoxItemMutation(), onSuccess: refresh, onError });
  const retry = useMutation({ ...retryDropBoxItemMutation(), onSuccess: refresh, onError });
  const undo = useMutation({
    ...undoPlacementMutation(),
    onSuccess: () => {
      toast.success("Undone: it is back in the drop box");
      void refresh();
    },
    onError,
  });

  const all = items.data ?? [];
  const shown = tabs.find((t) => t.value === tab)?.statuses;
  const list = shown ? all.filter((item) => shown.includes(item.status)) : all;

  return (
    <>
      <Tabs value={tab} onValueChange={(value) => setTab(String(value))}>
        <TabsList>
          {tabs.map((t) => (
            <TabsTrigger key={t.value} value={t.value}>
              {t.label}
              <Badge variant="secondary">
                {t.statuses ? all.filter((i) => t.statuses?.includes(i.status)).length : all.length}
              </Badge>
            </TabsTrigger>
          ))}
        </TabsList>
      </Tabs>

      {items.isError && <p className="text-sm text-destructive">{errorMessage(items.error)}</p>}
      {items.data && list.length === 0 ? (
        <EmptyState icon={InboxIcon} title="Nothing here" className="flex-none">
          {empty ?? "Drop files or links above, and the AI proposes where they go."}
        </EmptyState>
      ) : list.length > 0 ? (
        <ItemGroup className="gap-0 rounded-lg border">
          {list.map((item, i) => (
            <Fragment key={item.id}>
              {i > 0 && <ItemSeparator className="my-0" />}
              <Row
                item={item}
                onConfirm={() => confirm.mutate({ path: { id: item.id } }, { onError })}
                onChange={() => {
                  confirm.reset();
                  setDialog({ kind: "change", item });
                }}
                onPlace={() => {
                  place.reset();
                  setDialog({ kind: "place", item });
                }}
                onReject={() => reject.mutate({ path: { id: item.id } })}
                onRetry={() => retry.mutate({ path: { id: item.id } })}
                onUndo={(id) => undo.mutate({ path: { id } })}
              />
            </Fragment>
          ))}
        </ItemGroup>
      ) : null}

      {dialog?.kind === "change" && (
        <PlaceDialog
          item={dialog.item}
          title={`Change where ${dialog.item.name} goes`}
          action="Confirm"
          pending={confirm.isPending}
          error={confirm.error}
          onChoose={(body) => confirm.mutate({ path: { id: dialog.item.id }, body })}
          onClose={() => setDialog(undefined)}
        />
      )}
      {dialog?.kind === "place" && (
        <PlaceDialog
          item={dialog.item}
          title={`Place ${dialog.item.name}`}
          action="Place"
          pending={place.isPending}
          error={place.error}
          onChoose={(body) => place.mutate({ path: { id: dialog.item.id }, body })}
          onClose={() => setDialog(undefined)}
        />
      )}
    </>
  );
}

function Row({
  item,
  onConfirm,
  onChange,
  onPlace,
  onReject,
  onRetry,
  onUndo,
}: {
  item: ItemView;
  onConfirm: () => void;
  onChange: () => void;
  onPlace: () => void;
  onReject: () => void;
  onRetry: () => void;
  onUndo: (placement: string) => void;
}) {
  const proposal = item.proposal;
  const placement = item.placement;
  const pending = item.status === "proposed" && placement;
  const unplaced = UNPLACED.includes(item.status);
  const icon =
    item.guard === "by_hand" ? (
      <LockIcon />
    ) : item.kind === "link" ? (
      <LinkIcon />
    ) : item.kind === "folder" ? (
      <FolderIcon />
    ) : (
      <FileIcon />
    );
  const target = placement?.path ?? proposal?.path;
  const details = [proposal?.reason, item.note].filter(Boolean).join(" · ");
  return (
    <Item>
      <ItemMedia variant="icon" className="text-muted-foreground">
        {icon}
      </ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle className="flex-wrap">
          <span className="truncate">{item.name}</span>
          <Badge variant={unplaced ? "outline" : "secondary"}>
            {pending ? "Moved, pending" : statusLabel(item.status)}
          </Badge>
          {target && (
            <>
              <ArrowRightIcon className="size-4 text-muted-foreground" />
              <span className="truncate font-normal">{target}</span>
              {proposal && <Badge variant="outline">{percent(proposal.confidence)}</Badge>}
            </>
          )}
        </ItemTitle>
        {item.source_path && (
          <ItemDescription className="truncate">{item.source_path}</ItemDescription>
        )}
        {details && <ItemDescription>{details}</ItemDescription>}
        {item.error && unplaced && (
          <ItemDescription className="text-destructive">{item.error}</ItemDescription>
        )}
      </ItemContent>
      <ItemActions>
        {item.status === "proposed" && (
          <>
            <Button size="sm" onClick={onConfirm}>
              Confirm
            </Button>
            <Button size="sm" variant="outline" onClick={onChange}>
              Change
            </Button>
            {pending ? (
              <Button size="sm" variant="outline" onClick={() => onUndo(pending.id)}>
                Undo
              </Button>
            ) : (
              <Button size="sm" variant="outline" onClick={onReject}>
                Reject
              </Button>
            )}
          </>
        )}
        {unplaced && (
          <Button size="sm" variant="outline" onClick={onPlace}>
            Place
          </Button>
        )}
        {(unplaced || placement) && (
          <DropdownMenu>
            <DropdownMenuTrigger
              render={<Button variant="ghost" size="icon-sm" aria-label="More actions" />}
            >
              <MoreHorizontalIcon />
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {unplaced && (
                <DropdownMenuItem onClick={onRetry}>
                  <RotateCcwIcon />
                  Retry
                </DropdownMenuItem>
              )}
              {placement && (
                <DropdownMenuItem onClick={() => onUndo(placement.id)}>
                  <Undo2Icon />
                  Undo
                </DropdownMenuItem>
              )}
            </DropdownMenuContent>
          </DropdownMenu>
        )}
      </ItemActions>
    </Item>
  );
}
