import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link, createFileRoute } from "@tanstack/react-router";
import { PlusIcon } from "lucide-react";
import { toast } from "sonner";
import type { Team } from "organizer-sdk";
import {
  createTeamMutation,
  listMyTeamsOptions,
  listTeamsOptions,
  setPrimaryTeamMutation,
} from "organizer-sdk/react-query";
import { FormDialog } from "@/components/form-dialog";
import { ListRow } from "@/components/list-row";
import { RecipientIcon } from "@/components/recipient-icon";
import { SelectField, SettingField, SettingsCard } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage } from "@/lib/api";
import { TEAM_ROLE_LABELS, emailsOf, refreshTeams, teamSummary } from "@/lib/teams";
import { parseLimitGb } from "@/lib/users";

export const Route = createFileRoute("/_app/settings/teams/")({ component: Teams });

/**
 * Settings → Teams: the user's teams and their primary one; for the
 * superadmin, every team and New team.
 */
function Teams() {
  const { user } = Route.useRouteContext();
  return (
    <>
      <MyTeams />
      {user.is_superadmin && (
        <SuperadminGate lazy title="All teams">
          <AllTeams />
        </SuperadminGate>
      )}
    </>
  );
}

/** A team's row: its name (to its page), members and storage, and a badge. */
function TeamRow({ team, badge }: { team: Team; badge?: React.ReactNode }) {
  return (
    <ListRow
      icon={<RecipientIcon kind="team" label={team.name} />}
      title={team.name}
      detail={teamSummary(team)}
    >
      {badge}
      <Button
        variant="outline"
        size="sm"
        render={<Link to="/settings/teams/$teamId" params={{ teamId: team.id }} />}
      >
        Open
      </Button>
    </ListRow>
  );
}

/** The user's teams, their role in each, and the primary one their defaults come from. */
function MyTeams() {
  const teams = useQuery(listMyTeamsOptions());
  const primary = useMutation({
    ...setPrimaryTeamMutation(),
    onSuccess: () => {
      refreshTeams();
      toast.success("Primary team saved");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const list = teams.data ?? [];
  return (
    <SettingsCard
      title="Your teams"
      description="Teams share drives and settings. Your defaults come from your primary team."
      error={teams.error}
    >
      {teams.data && list.length === 0 ? (
        <p className="text-sm text-muted-foreground">You're not in a team.</p>
      ) : (
        <div className="flex flex-col gap-4">
          {list.length > 1 && (
            <SelectField
              id="primary-team"
              label="Primary team"
              description="Where your defaults come from, when nothing is locked."
              items={list.map((t) => ({ value: t.id, label: t.name }))}
              value={list.find((t) => t.primary)?.id ?? ""}
              onChange={(team_id) => team_id && primary.mutate({ body: { team_id } })}
            />
          )}
          <div className="divide-y">
            {list.map((team) => (
              <TeamRow
                key={team.id}
                team={team}
                badge={
                  <>
                    {team.primary && list.length > 1 && <Badge variant="secondary">Primary</Badge>}
                    {team.my_role && (
                      <Badge variant="outline">{TEAM_ROLE_LABELS[team.my_role]}</Badge>
                    )}
                  </>
                }
              />
            ))}
          </div>
        </div>
      )}
    </SettingsCard>
  );
}

/** The superadmin's list of every team. */
function AllTeams() {
  const teams = useQuery(listTeamsOptions());
  const [making, setMaking] = useState(false);
  return (
    <SettingsCard
      title="All teams"
      description="Each team has its managers, its shared drives with a storage limit for all of them, and its defaults."
      action={
        <Button variant="outline" onClick={() => setMaking(true)}>
          <PlusIcon />
          New team
        </Button>
      }
      error={teams.error}
    >
      {teams.data?.length === 0 ? (
        <p className="text-sm text-muted-foreground">No teams yet.</p>
      ) : (
        <div className="divide-y">
          {teams.data?.map((team) => (
            <TeamRow key={team.id} team={team} />
          ))}
        </div>
      )}
      {making && <NewTeamDialog onClose={() => setMaking(false)} />}
    </SettingsCard>
  );
}

/** "New team": its name, its first managers and members, a storage limit. */
function NewTeamDialog({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState("");
  const [managers, setManagers] = useState("");
  const [members, setMembers] = useState("");
  const [limit, setLimit] = useState("");
  const [invalid, setInvalid] = useState(false);
  const create = useMutation({
    ...createTeamMutation(),
    onSuccess: (team) => {
      refreshTeams();
      toast.success(`${team.name} is made`);
      onClose();
    },
  });
  return (
    <FormDialog
      title="New team"
      description="Its managers add members, make its shared drives and set its defaults."
      action="Create team"
      pending={create.isPending}
      error={invalid ? "Expected a number of GB, or nothing for no limit" : create.error}
      onSubmit={() => {
        const storage = parseLimitGb(limit);
        setInvalid(storage === undefined);
        if (storage === undefined) return;
        create.mutate({
          body: {
            name: name.trim(),
            managers: emailsOf(managers),
            members: emailsOf(members),
            storage_limit: storage,
          },
        });
      }}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="team-name">Name</FieldLabel>
        <Input
          id="team-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
          required
        />
      </Field>
      <SettingField id="team-managers" label="Managers" description="Emails, one per line.">
        <Textarea
          id="team-managers"
          value={managers}
          onChange={(e) => setManagers(e.target.value)}
        />
      </SettingField>
      <SettingField id="team-members" label="Members" description="Emails, one per line.">
        <Textarea id="team-members" value={members} onChange={(e) => setMembers(e.target.value)} />
      </SettingField>
      <SettingField
        id="team-limit"
        label="Storage limit in GB"
        description="For all its drives together. Empty for no limit."
      >
        <Input
          id="team-limit"
          inputMode="decimal"
          placeholder="No limit"
          value={limit}
          onChange={(e) => setLimit(e.target.value)}
        />
      </SettingField>
    </FormDialog>
  );
}
