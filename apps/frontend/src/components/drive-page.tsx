import { useQuery } from "@tanstack/react-query";
import { LockIcon } from "lucide-react";
import { getDriveOptions } from "organizer-sdk/react-query";
import { EmptyState } from "@/components/empty-state";
import { FolderView } from "@/components/folder-view";
import { PageHeader } from "@/components/page-header";
import { errorMessage } from "@/lib/api";
import { unavailableText } from "@/lib/backends";
import { entryAt } from "@/lib/drive-files";
import { driveUrl } from "@/lib/drives";

/**
 * A shared drive's folder (`/drives/<id>` and `/drives/<id>/<path>`): the
 * folder view of My drive, in that drive. The server looks paths up in
 * My drive only, so the path is walked down from the drive's root.
 */
export function SharedDrivePage({ id, path, me }: { id: string; path: string; me: string }) {
  // Asked again while it is moved: the move's end is no change of its files.
  const drive = useQuery({
    ...getDriveOptions({ path: { id } }),
    refetchInterval: (query) => (query.state.data?.moving_to ? 5000 : false),
  });
  // A drive whose storage fails or is locked has no folder to show.
  const unavailable = drive.data && unavailableText(drive.data);
  const folder = useQuery({
    queryKey: ["drive-entry", id, path, drive.data?.root_id],
    enabled: drive.data !== undefined && !unavailable,
    queryFn: () => entryAt(drive.data!, path),
  });
  if (drive.data && !unavailable && folder.data?.kind === "folder") {
    return <FolderView folder={folder.data} drive={drive.data} me={me} />;
  }
  const error = drive.error ?? folder.error;
  const parts = path.split("/").filter(Boolean);
  return (
    <>
      <PageHeader
        crumbs={[
          { label: drive.data?.name ?? "Drive", to: driveUrl({ id, kind: "shared" }, "/") },
          ...(parts.length ? [{ label: path }] : []),
        ]}
      />
      {unavailable ? (
        <div className="flex flex-1 flex-col p-4">
          <EmptyState icon={LockIcon} title={unavailable}>
            {drive.data?.remote
              ? `${drive.data.name} is on another server, ${drive.data.remote.server}. It opens again once that server lets this one in.`
              : drive.data?.backend.owner
                ? `${drive.data.name} is stored on ${drive.data.backend.owner}'s own storage. Its files are kept; it opens again once that storage can be used.`
                : `Its files are kept; ${drive.data?.name} opens again once its storage can be used.`}
          </EmptyState>
        </div>
      ) : (
        error && <p className="p-4 text-sm text-destructive">{errorMessage(error)}</p>
      )}
      {folder.data?.kind === "file" && (
        <p className="p-4 text-sm text-muted-foreground">That's a file, not a folder.</p>
      )}
    </>
  );
}
