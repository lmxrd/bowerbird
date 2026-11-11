import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { toast } from "sonner";
import { getRetentionOptions, updateRetentionMutation } from "organizer-sdk/react-query";
import { LevelSettingsCard, useMySettings } from "@/components/levels";
import { RetentionForm } from "@/components/retention-form";
import { SettingsCard } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { queryClient } from "@/lib/api";

export const Route = createFileRoute("/_app/settings/retention")({ component: TrashAndVersions });

function TrashAndVersions() {
  const { user } = Route.useRouteContext();
  const mine = useQuery(getRetentionOptions());
  const levels = useMySettings();
  const save = useMutation({
    ...updateRetentionMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Saved");
    },
  });
  return (
    <>
      <SettingsCard
        title="Trash and versions"
        description="How long what you delete and the earlier versions of your files are kept. Fewer days apply within the hour: what falls outside is deleted for good."
        error={mine.error}
      >
        {mine.data && (
          <RetentionForm
            data={mine.data}
            levels={levels}
            saving={save.isPending}
            saveError={save.isError ? save.error : undefined}
            onSave={(body) => save.mutate({ body })}
          />
        )}
      </SettingsCard>
      {user.is_superadmin && (
        <SuperadminGate lazy title="For everyone">
          <LevelSettingsCard
            scope={{ level: "superadmin" }}
            prefixes={["retention."]}
            title="For everyone"
            description="What every user and drive has: a default they may change, locked, or held to a range (such as at least 7 days in the trash)."
          />
        </SuperadminGate>
      )}
    </>
  );
}
