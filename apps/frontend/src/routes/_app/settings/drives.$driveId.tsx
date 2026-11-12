import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { ArrowRightLeftIcon } from "lucide-react";
import { toast } from "sonner";
import type { Drive } from "organizer-sdk";
import {
  backendChoicesOptions,
  getDriveOptions,
  moveDriveMutation,
} from "organizer-sdk/react-query";
import { MoveDriveDialog, refreshBackends } from "@/components/backends";
import { LevelSettingsCard } from "@/components/levels";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import { errorMessage } from "@/lib/api";
import { backupNote, movingText, storedOn, unavailableText } from "@/lib/backends";
import { driveUrl } from "@/lib/drives";
import { can } from "@/lib/roles";

/**
 * A shared drive's own settings (docs/M7B_DESIGN.md §4): its public links
 * and its trash and versions, within what its team and the superadmin
 * leave, and where it is stored (docs/M7C_DESIGN.md §1). For its managers,
 * from the drive's page.
 */
export const Route = createFileRoute("/_app/settings/drives/$driveId")({
  component: DriveSettings,
});

function DriveSettings() {
  const { driveId } = Route.useParams();
  const drive = useQuery({
    ...getDriveOptions({ path: { id: driveId } }),
    refetchInterval: (query) => (query.state.data?.moving_to ? 5000 : false),
  });
  if (drive.isError) {
    return <p className="text-sm text-destructive">{errorMessage(drive.error)}</p>;
  }
  const data = drive.data;
  if (!data) return null;
  const back = (
    <Link to={driveUrl(data, "/")} className="underline underline-offset-4">
      Back to {data.name}
    </Link>
  );
  if (data.kind !== "shared" || !can(data.my_role, "manage")) {
    return (
      <SettingsCard title={`Settings of ${data.name}`} description={back}>
        <p className="text-sm text-muted-foreground">
          {data.kind === "shared"
            ? "Only who manages this drive changes its settings."
            : "My drive follows your own settings: Trash and versions, and your teams' links."}
        </p>
      </SettingsCard>
    );
  }
  return (
    <>
      <LevelSettingsCard
        scope={{ level: "drive", id: data.id }}
        title={`Settings of ${data.name}`}
        description={
          <>
            Its public links, how long its trash and versions are kept, whether members may mirror
            it to their own storage (forbidding stops their mirrors), and whether it may be shared
            with people on other servers (forbidding stops those shares), within what{" "}
            {data.team_name ? `${data.team_name} and the superadmin` : "the superadmin"} left open.{" "}
            {back}
          </>
        }
      />
      <StorageCard drive={data} />
    </>
  );
}

/** Where the drive is stored, and "Move to other storage…". */
function StorageCard({ drive }: { drive: Drive }) {
  const [moving, setMoving] = useState(false);
  const choices = useQuery({
    ...backendChoicesOptions({ query: drive.team_id ? { team_id: drive.team_id } : {} }),
    enabled: moving,
  });
  const move = useMutation({
    ...moveDriveMutation(),
    onSuccess: (moved) => {
      refreshBackends();
      toast.success(movingText(moved) ?? "Moving");
      setMoving(false);
    },
  });
  const state = movingText(drive) ?? unavailableText(drive);
  return (
    <SettingsCard
      title="Storage"
      description={[storedOn(drive), backupNote(drive.backend)].filter(Boolean).join(". ")}
      action={
        <Button
          size="sm"
          variant="outline"
          disabled={!!drive.moving_to}
          onClick={() => setMoving(true)}
        >
          <ArrowRightLeftIcon />
          Move to other storage…
        </Button>
      }
    >
      <p className="text-sm text-muted-foreground">
        {state ?? "Moving copies its files, trash and versions; it is read-only meanwhile."}
      </p>
      {moving && (
        <MoveDriveDialog
          drive={drive}
          choices={choices.data}
          pending={move.isPending}
          error={move.error}
          onMove={(backend_id) => move.mutate({ path: { id: drive.id }, body: { backend_id } })}
          onClose={() => setMoving(false)}
        />
      )}
    </SettingsCard>
  );
}
