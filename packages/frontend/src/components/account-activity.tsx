import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { getMyActivityOptions } from "organizer-sdk/react-query";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { ACTIVITY_PAGE, activityActor, activityText } from "@/lib/activity";
import { errorMessage } from "@/lib/api";
import { formatDateTime } from "@/lib/format";

/**
 * Settings → Security → Account activity: what a superadmin, a team's
 * manager or the server's command line did to the account, the newest
 * first, a page at a time.
 */
export function AccountActivity() {
  // Each page starts after the last entry of the one before.
  const [pages, setPages] = useState<(string | undefined)[]>([undefined]);
  return (
    <SettingsCard
      title="Account activity"
      description="What your administrator or your teams' managers did to your account: when, what and who."
    >
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead className="w-44">When</TableHead>
            <TableHead>What</TableHead>
            <TableHead className="w-56">Who</TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {pages.map((before, i) => (
            <ActivityPage
              key={before ?? "first"}
              before={before}
              first={i === 0}
              last={i === pages.length - 1}
              onMore={(after) => setPages([...pages, after])}
            />
          ))}
        </TableBody>
      </Table>
    </SettingsCard>
  );
}

function ActivityPage({
  before,
  first,
  last,
  onMore,
}: {
  before: string | undefined;
  first: boolean;
  /** The last page shown: it offers the next. */
  last: boolean;
  onMore: (after: string) => void;
}) {
  const page = useQuery(
    getMyActivityOptions({ query: { limit: ACTIVITY_PAGE, ...(before ? { before } : {}) } }),
  );
  const entries = page.data ?? [];
  const message = (text: string, destructive = false) => (
    <TableRow>
      <TableCell colSpan={3} className={destructive ? "text-destructive" : "text-muted-foreground"}>
        {text}
      </TableCell>
    </TableRow>
  );
  return (
    <>
      {page.isError && message(errorMessage(page.error), true)}
      {first && page.data?.length === 0 && message("Nothing yet.")}
      {entries.map((entry) => (
        <TableRow key={entry.id}>
          <TableCell className="text-muted-foreground">{formatDateTime(entry.at)}</TableCell>
          <TableCell className="whitespace-normal">{activityText(entry)}</TableCell>
          <TableCell className="whitespace-normal text-muted-foreground">
            {activityActor(entry)}
          </TableCell>
        </TableRow>
      ))}
      {last && entries.length === ACTIVITY_PAGE && (
        <TableRow>
          <TableCell colSpan={3}>
            <Button
              variant="outline"
              size="sm"
              onClick={() => onMore(entries[entries.length - 1].id)}
            >
              Show more
            </Button>
          </TableCell>
        </TableRow>
      )}
    </>
  );
}
