import { createFileRoute } from "@tanstack/react-router";
import { SharedDrivePage } from "@/components/drive-page";
import { splatPath } from "@/lib/drives";

/** A folder of a shared drive by its path: `/drives/<id>/Plans/2027`. */
export const Route = createFileRoute("/_app/drives/$driveId/$")({ component: DriveFolder });

function DriveFolder() {
  const { user } = Route.useRouteContext();
  const { driveId, _splat } = Route.useParams();
  return <SharedDrivePage id={driveId} path={splatPath(_splat)} me={user.id} />;
}
