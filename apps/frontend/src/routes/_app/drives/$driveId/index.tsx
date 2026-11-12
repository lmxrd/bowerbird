import { createFileRoute } from "@tanstack/react-router";
import { SharedDrivePage } from "@/components/drive-page";

/** A shared drive's root: `/drives/<id>`. */
export const Route = createFileRoute("/_app/drives/$driveId/")({ component: DriveRoot });

function DriveRoot() {
  const { user } = Route.useRouteContext();
  const { driveId } = Route.useParams();
  return <SharedDrivePage id={driveId} path="/" me={user.id} />;
}
