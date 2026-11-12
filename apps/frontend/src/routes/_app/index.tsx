import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { getRootOptions } from "organizer-sdk/react-query";
import { FolderView } from "@/components/folder-view";
import { PageHeader } from "@/components/page-header";
import { errorMessage } from "@/lib/api";
import { drivesOptions, myDrive } from "@/lib/drive-files";
import { MY_DRIVE } from "@/lib/drives";

/** My drive's root. */
export const Route = createFileRoute("/_app/")({ component: Root });

function Root() {
  const { user } = Route.useRouteContext();
  const root = useQuery(getRootOptions());
  const drives = useQuery(drivesOptions());
  if (root.data) return <FolderView folder={root.data} drive={myDrive(drives.data)} me={user.id} />;
  return (
    <>
      <PageHeader crumbs={[{ label: MY_DRIVE.name }]} />
      {root.isError && <p className="p-4 text-sm text-destructive">{errorMessage(root.error)}</p>}
    </>
  );
}
