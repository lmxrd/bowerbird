import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { getFileByPathOptions } from "organizer-sdk/react-query";
import { FolderView } from "@/components/folder-view";
import { PageHeader } from "@/components/page-header";
import { errorMessage } from "@/lib/api";
import { drivesOptions, myDrive } from "@/lib/drive-files";
import { MY_DRIVE } from "@/lib/drives";

/** A folder of My drive by its path: `/files/Documents/Taxes`. */
export const Route = createFileRoute("/_app/files/$")({ component: Folder });

function Folder() {
  const { user } = Route.useRouteContext();
  const { _splat } = Route.useParams();
  const path = `/${_splat ?? ""}`;
  const folder = useQuery(getFileByPathOptions({ query: { path } }));
  const drives = useQuery(drivesOptions());
  if (folder.data?.kind === "folder") {
    return <FolderView folder={folder.data} drive={myDrive(drives.data)} me={user.id} />;
  }
  return (
    <>
      <PageHeader crumbs={[{ label: MY_DRIVE.name, to: "/" }, { label: path }]} />
      {folder.isError && (
        <p className="p-4 text-sm text-destructive">{errorMessage(folder.error)}</p>
      )}
      {folder.data?.kind === "file" && (
        <p className="p-4 text-sm text-muted-foreground">That's a file, not a folder.</p>
      )}
    </>
  );
}
