import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { FileIcon, FolderIcon, SearchIcon } from "lucide-react";
import type { Drive, Hit } from "organizer-sdk/types";
import { searchOptions } from "organizer-sdk/react-query";
import { PageHeader } from "@/components/page-header";
import { Badge } from "@/components/ui/badge";
import {
  Item,
  ItemContent,
  ItemDescription,
  ItemGroup,
  ItemMedia,
  ItemSeparator,
  ItemTitle,
} from "@/components/ui/item";
import { errorMessage } from "@/lib/api";
import { drivesOptions } from "@/lib/drive-files";
import { hitUrl } from "@/lib/drives";
import { splitHighlights } from "@/lib/highlight";
import { Fragment } from "react";

export const Route = createFileRoute("/_app/search")({
  validateSearch: (search: Record<string, unknown>): { q: string } => ({
    q: typeof search.q === "string" ? search.q : "",
  }),
  component: Search,
});

function Search() {
  const { q } = Route.useSearch();
  const results = useQuery({
    ...searchOptions({ query: { q, semantic: true } }),
    enabled: q.trim() !== "",
  });
  const drives = useQuery(drivesOptions());
  return (
    <>
      <PageHeader crumbs={[{ label: "Search" }]} />
      <div className="flex flex-1 flex-col gap-4 p-4">
        <h1 className="text-lg font-medium">{q ? `Results for “${q}”` : "Search"}</h1>
        {results.isError && (
          <p className="text-sm text-destructive">{errorMessage(results.error)}</p>
        )}
        {!q || results.data?.length === 0 ? (
          <div className="flex flex-col items-center justify-center gap-2 rounded-lg border border-dashed p-10 text-center">
            <SearchIcon className="size-6 text-muted-foreground" />
            <p className="font-medium">{q ? "Nothing found" : "Search your files"}</p>
            <p className="text-sm text-muted-foreground">
              Names, folders and the text of your files, by their words or the start of them.
            </p>
          </div>
        ) : results.data ? (
          <ItemGroup className="gap-0 rounded-lg border">
            {results.data.map((hit, i) => (
              <Fragment key={hit.node_id}>
                {i > 0 && <ItemSeparator className="my-0" />}
                <Result hit={hit} drives={drives.data} />
              </Fragment>
            ))}
          </ItemGroup>
        ) : null}
      </div>
    </>
  );
}

/**
 * A found file or folder; it opens the folder it is (in), in its drive.
 * Outside My drive, where it is starts with the drive's name.
 */
function Result({ hit, drives }: { hit: Hit; drives: Drive[] | undefined }) {
  const folder = hit.kind === "folder";
  const drive = drives?.find((d) => d.id === hit.drive_id);
  const parent = [
    ...(drive?.kind === "shared" ? [drive.name] : []),
    ...hit.path.split("/").filter(Boolean),
  ];
  const to = hitUrl(hit, drives);
  return (
    <Item render={<Link to={to} />}>
      <ItemMedia variant="icon" className="text-muted-foreground">
        {folder ? <FolderIcon /> : <FileIcon />}
      </ItemMedia>
      <ItemContent className="min-w-0">
        <ItemTitle>
          <span className="truncate">{hit.name}</span>
          {hit.semantic && <Badge variant="secondary">Related</Badge>}
        </ItemTitle>
        <ItemDescription>{parent.length ? parent.join(" / ") : "Top"}</ItemDescription>
        {hit.snippet && (
          <ItemDescription>
            {splitHighlights(hit.snippet).map((piece, i) =>
              piece.match ? (
                <mark key={i} className="rounded-sm bg-primary/15 px-0.5 text-foreground">
                  {piece.text}
                </mark>
              ) : (
                <Fragment key={i}>{piece.text}</Fragment>
              ),
            )}
          </ItemDescription>
        )}
      </ItemContent>
    </Item>
  );
}
