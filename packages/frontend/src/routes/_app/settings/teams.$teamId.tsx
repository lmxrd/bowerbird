import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link, createFileRoute, useNavigate } from "@tanstack/react-router";
import { HardDriveIcon, PencilIcon, PlusIcon, Trash2Icon, UserPlusIcon, XIcon } from "lucide-react";
import { toast } from "sonner";
import type { Role, Team, TeamMember, TeamRole } from "organizer-sdk";
import {
  addTeamMemberMutation,
  backendChoicesOptions,
  createSharedDriveMutation,
  createTeamDriveMutation,
  deleteTeamMutation,
  getDriveAccessOptions,
  getTeamOptions,
  listGlobalBackendsOptions,
  listTeamsOptions,
  listUsersOptions,
  removeTeamMemberAsSuperadminMutation,
  removeTeamMemberMutation,
  setTeamMemberAsSuperadminMutation,
  updateTeamMemberMutation,
  updateTeamMutation,
} from "organizer-sdk/react-query";
import { StoredOnField, globalChoices } from "@/components/backends";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { FormDialog } from "@/components/form-dialog";
import { KindIcon } from "@/components/kind-icon";
import { LevelSettingsCard } from "@/components/levels";
import { ListRow } from "@/components/list-row";
import { RenameDialog, StorageLimitDialog } from "@/components/name-dialogs";
import { OptionSelect, RoleSelect } from "@/components/option-select";
import { RecipientIcon } from "@/components/recipient-icon";
import { RowMenu } from "@/components/row-menu";
import { SettingField, SettingsCard } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage } from "@/lib/api";
import { DISK_ID, chosenBackend } from "@/lib/backends";
import { formatDate } from "@/lib/format";
import { ROLE_LABELS } from "@/lib/roles";
import { TEAM_ROLES, emailsOf, refreshTeams } from "@/lib/teams";
import { formatUsage, usagePercent } from "@/lib/users";

export const Route = createFileRoute("/_app/settings/teams/$teamId")({ component: TeamPage });

/**
 * A team's page: its storage, members, drives and defaults. Its members
 * see it through their endpoints (its managers change it); the superadmin
 * who is not in it through theirs, after a step-up.
 */
function TeamPage() {
  const { teamId } = Route.useParams();
  const { user } = Route.useRouteContext();
  const mine = useQuery({ ...getTeamOptions({ path: { id: teamId } }), retry: false });
  if (mine.data) return <TeamView team={mine.data} superadmin={user.is_superadmin} me={user.id} />;
  if (mine.isError && user.is_superadmin) {
    return (
      <SuperadminGate>
        <SuperadminTeam id={teamId} me={user.id} />
      </SuperadminGate>
    );
  }
  if (mine.isError) return <p className="text-sm text-destructive">{errorMessage(mine.error)}</p>;
  return null;
}

/** A team the superadmin is not in, from their list of every team. */
function SuperadminTeam({ id, me }: { id: string; me: string }) {
  const teams = useQuery(listTeamsOptions());
  const team = teams.data?.find((t) => t.id === id);
  if (teams.isError) return <p className="text-sm text-destructive">{errorMessage(teams.error)}</p>;
  if (teams.data && !team) return <p className="text-sm text-muted-foreground">No such team.</p>;
  return team ? <TeamView team={team} superadmin me={me} /> : null;
}

type Dialog =
  | { kind: "rename" }
  | { kind: "limit" }
  | { kind: "delete" }
  | { kind: "members" }
  | { kind: "drive" };

const failed = (error: unknown) => toast.error(errorMessage(error));

function TeamView({ team, superadmin, me }: { team: Team; superadmin: boolean; me: string }) {
  const navigate = useNavigate();
  const [dialog, setDialog] = useState<Dialog>();
  const close = () => setDialog(undefined);
  const manages = team.my_role === "manager";
  // A team's managers change it through the team's endpoints, the
  // superadmin (not managing it) through theirs.
  const mayChange = manages || superadmin;
  const path = { id: team.id };
  const done = { onSuccess: refreshTeams, onError: failed };
  const update = useMutation({ ...updateTeamMemberMutation(), ...done });
  const remove = useMutation({ ...removeTeamMemberMutation(), ...done });
  const setAsSuperadmin = useMutation({ ...setTeamMemberAsSuperadminMutation(), ...done });
  const removeAsSuperadmin = useMutation({ ...removeTeamMemberAsSuperadminMutation(), ...done });
  const rename = useMutation({
    ...updateTeamMutation(),
    onSuccess: () => {
      refreshTeams();
      close();
    },
  });
  const limit = useMutation({
    ...updateTeamMutation(),
    onSuccess: () => {
      refreshTeams();
      toast.success("Storage limit saved");
      close();
    },
  });
  const deleteTeam = useMutation({
    ...deleteTeamMutation(),
    onSuccess: () => {
      refreshTeams();
      toast.success(`${team.name} is deleted`);
      void navigate({ to: "/settings/teams" });
    },
    onError: failed,
  });
  const setRole = (member: TeamMember, role: TeamRole) =>
    manages
      ? update.mutate({ path: { ...path, user_id: member.user_id }, body: { role } })
      : setAsSuperadmin.mutate({ path: { ...path, user_id: member.user_id }, body: { role } });
  const takeOut = (member: TeamMember) =>
    manages
      ? remove.mutate({ path: { ...path, user_id: member.user_id } })
      : removeAsSuperadmin.mutate({ path: { ...path, user_id: member.user_id } });
  const percent = usagePercent(team.used, team.storage_limit);
  return (
    <>
      <SettingsCard
        title={team.name}
        description={`Storage: ${formatUsage(team.used, team.storage_limit)} across its drives${team.storage_limit == null ? ", no limit" : ""}.`}
        action={
          superadmin && (
            <RowMenu
              label="Team actions"
              actions={[
                {
                  label: "Rename",
                  icon: <PencilIcon />,
                  onClick: () => setDialog({ kind: "rename" }),
                },
                {
                  label: "Storage limit…",
                  icon: <HardDriveIcon />,
                  onClick: () => setDialog({ kind: "limit" }),
                },
                {
                  label: "Delete…",
                  icon: <Trash2Icon />,
                  destructive: true,
                  onClick: () => setDialog({ kind: "delete" }),
                },
              ]}
            />
          )
        }
      >
        {percent !== undefined ? (
          <Progress value={percent} aria-label="Storage used" />
        ) : (
          <p className="text-sm text-muted-foreground">
            {superadmin ? "No limit: set one in the menu." : "No limit."}
          </p>
        )}
      </SettingsCard>

      <SettingsCard
        title="Members"
        description="Managers add and remove members, make the team's shared drives and set its defaults."
        action={
          mayChange && (
            <Button variant="outline" onClick={() => setDialog({ kind: "members" })}>
              <UserPlusIcon />
              Add members
            </Button>
          )
        }
      >
        {team.members.length === 0 ? (
          <p className="text-sm text-muted-foreground">No members yet.</p>
        ) : (
          <div className="divide-y">
            {team.members.map((member) => (
              <ListRow
                key={member.user_id}
                icon={<RecipientIcon kind="user" label={member.email} />}
                title={member.user_id === me ? `${member.email} (you)` : member.email}
                detail={`Joined ${formatDate(member.joined_at)}`}
              >
                <OptionSelect
                  options={TEAM_ROLES}
                  value={member.role}
                  disabled={!mayChange}
                  onChange={(role) => setRole(member, role)}
                  label={`Role of ${member.email}`}
                  className="w-32"
                />
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={`Take ${member.email} out`}
                  disabled={!mayChange}
                  onClick={() => takeOut(member)}
                >
                  <XIcon />
                </Button>
              </ListRow>
            ))}
          </div>
        )}
      </SettingsCard>

      <SettingsCard
        title="Drives"
        description="The team's shared drives: its managers manage them, its members get the role each drive gives the team."
        action={
          mayChange && (
            <Button variant="outline" onClick={() => setDialog({ kind: "drive" })}>
              <PlusIcon />
              New drive
            </Button>
          )
        }
      >
        {team.drives.length === 0 ? (
          <p className="text-sm text-muted-foreground">No drives yet.</p>
        ) : (
          <div className="divide-y">
            {team.drives.map((drive) => (
              <ListRow
                key={drive.id}
                icon={<KindIcon kind="folder" />}
                title={
                  <Link
                    to="/drives/$driveId"
                    params={{ driveId: drive.id }}
                    className="underline-offset-4 hover:underline"
                  >
                    {drive.name}
                  </Link>
                }
                detail={team.my_role && <TeamDriveRole driveId={drive.id} teamId={team.id} />}
              />
            ))}
          </div>
        )}
      </SettingsCard>

      {manages ? (
        <LevelSettingsCard
          scope={{ level: "team", id: team.id }}
          title="Team defaults"
          description="What the team's members have, within what the superadmin left open: a default they may change, locked, or a range. Its drives follow its rules."
        />
      ) : (
        superadmin && (
          <SettingsCard
            title="Team defaults"
            description="The team's managers set them, within what you leave open."
          >
            <p className="text-sm text-muted-foreground">
              {team.members.some((m) => m.role === "manager")
                ? "Lock a setting for everyone on its own page to keep it out of their hands."
                : "The team has no manager yet: make one of its members a manager."}
            </p>
          </SettingsCard>
        )
      )}

      {dialog?.kind === "rename" && (
        <RenameDialog
          initial={team.name}
          pending={rename.isPending}
          error={rename.error}
          onSave={(name) => rename.mutate({ path, body: { name } })}
          onClose={close}
        />
      )}
      {dialog?.kind === "limit" && (
        <StorageLimitDialog
          description={`${team.name}'s drives use ${formatUsage(team.used, null)} together. Writes past the limit are refused.`}
          limit={team.storage_limit}
          pending={limit.isPending}
          error={limit.error}
          onSave={(storage_limit) => limit.mutate({ path, body: { storage_limit } })}
          onClose={close}
        />
      )}
      {dialog?.kind === "delete" && (
        <ConfirmDialog
          title={`Delete ${team.name}?`}
          description="Its members leave it, and its drives lose its managers and its role. The drives and their files stay."
          action="Delete"
          confirmText={team.name}
          onConfirm={() => deleteTeam.mutate({ path })}
          onClose={close}
        />
      )}
      {dialog?.kind === "members" && (
        <AddMembersDialog team={team} asManager={manages} onClose={close} />
      )}
      {dialog?.kind === "drive" && (
        <NewTeamDriveDialog team={team} asManager={manages} onClose={close} />
      )}
    </>
  );
}

/** The role a team drive gives the team's members: "Members: Editor". */
function TeamDriveRole({ driveId, teamId }: { driveId: string; teamId: string }) {
  const access = useQuery({ ...getDriveAccessOptions({ path: { id: driveId } }), retry: false });
  const role = access.data?.grants.find((g) => g.team?.id === teamId)?.role;
  return role ? `Members: ${ROLE_LABELS[role]}` : null;
}

/** "Add members": emails and the role they get. */
function AddMembersDialog({
  team,
  asManager,
  onClose,
}: {
  team: Team;
  /** Through the team's endpoints (a manager), else the superadmin's. */
  asManager: boolean;
  onClose: () => void;
}) {
  const [emails, setEmails] = useState("");
  const [role, setRole] = useState<TeamRole>("member");
  const [error, setError] = useState<unknown>();
  const [pending, setPending] = useState(false);
  const users = useQuery({ ...listUsersOptions(), enabled: !asManager });
  const add = useMutation(addTeamMemberMutation());
  const set = useMutation(setTeamMemberAsSuperadminMutation());
  const submit = async () => {
    setPending(true);
    setError(undefined);
    try {
      for (const email of emailsOf(emails)) {
        if (asManager) {
          await add.mutateAsync({ path: { id: team.id }, body: { email, role } });
        } else {
          const user = users.data?.find((u) => u.email.toLowerCase() === email.toLowerCase());
          if (!user) throw new Error(`No user with the email ${email}`);
          await set.mutateAsync({ path: { id: team.id, user_id: user.id }, body: { role } });
        }
      }
      refreshTeams();
      onClose();
    } catch (e) {
      refreshTeams();
      setError(e);
    } finally {
      setPending(false);
    }
  };
  return (
    <FormDialog
      title={`Add members to ${team.name}`}
      action="Add"
      pending={pending}
      error={error}
      onSubmit={() => void submit()}
      onClose={onClose}
    >
      <SettingField id="new-members" label="Emails" description="One or several, one per line.">
        <Textarea
          id="new-members"
          value={emails}
          onChange={(e) => setEmails(e.target.value)}
          autoFocus
          required
        />
      </SettingField>
      <Field orientation="horizontal">
        <FieldLabel htmlFor="new-members-role">As</FieldLabel>
        <OptionSelect
          options={TEAM_ROLES}
          value={role}
          onChange={setRole}
          label="Their role in the team"
          className="w-32"
        />
      </Field>
    </FormDialog>
  );
}

/** The roles a team drive may give the team's members. */
const DRIVE_ROLES: Role[] = ["editor", "viewer", "upload_only"];

/** "New drive" of a team: its name and the role its members get. */
function NewTeamDriveDialog({
  team,
  asManager,
  onClose,
}: {
  team: Team;
  asManager: boolean;
  onClose: () => void;
}) {
  const [name, setName] = useState("");
  const [role, setRole] = useState<Role>("editor");
  const [storedOn, setStoredOn] = useState(DISK_ID);
  // A team's managers choose of what they may use; the superadmin of every global backend.
  const choices = useQuery({
    ...backendChoicesOptions({ query: { team_id: team.id } }),
    enabled: asManager,
  });
  const backends = useQuery({ ...listGlobalBackendsOptions(), enabled: !asManager });
  const backend_id = chosenBackend(storedOn) ?? null;
  const made = (drive: string) => {
    refreshTeams();
    toast.success(`${drive} is made`);
    onClose();
  };
  const create = useMutation({ ...createTeamDriveMutation(), onSuccess: (d) => made(d.name) });
  const createAsSuperadmin = useMutation({
    ...createSharedDriveMutation(),
    onSuccess: (d) => made(d.drive.name),
  });
  const mutation = asManager ? create : createAsSuperadmin;
  return (
    <FormDialog
      title={`New drive of ${team.name}`}
      description="The team's managers manage it; its storage counts toward the team's limit."
      action="Create drive"
      pending={mutation.isPending}
      error={mutation.error}
      onSubmit={() =>
        asManager
          ? create.mutate({ path: { id: team.id }, body: { name: name.trim(), role, backend_id } })
          : createAsSuperadmin.mutate({
              body: { name: name.trim(), team_id: team.id, backend_id },
            })
      }
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="team-drive-name">Name</FieldLabel>
        <Input
          id="team-drive-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
          required
        />
      </Field>
      <StoredOnField
        choices={asManager ? choices.data : globalChoices(backends.data)}
        value={storedOn}
        onChange={setStoredOn}
      />
      {asManager && (
        <Field orientation="horizontal">
          <FieldLabel htmlFor="team-drive-role">Members get</FieldLabel>
          <RoleSelect roles={DRIVE_ROLES} value={role} onChange={setRole} label="Members get" />
        </Field>
      )}
    </FormDialog>
  );
}
