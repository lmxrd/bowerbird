import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { CloudIcon, HardDriveIcon, LockIcon } from "lucide-react";
import { toast } from "sonner";
import type { Backend, BackendChoice, BackendTest, Drive } from "organizer-sdk";
import {
  addBackendMutation,
  addGlobalBackendMutation,
  changeBackendMutation,
  changeGlobalBackendMutation,
  getEncryptionOptions,
  listTeamsOptions,
  testBackendMutation,
  testBackendSettingsMutation,
  testGlobalBackendMutation,
  testGlobalBackendSettingsMutation,
} from "organizer-sdk/react-query";
import { FormDialog } from "@/components/form-dialog";
import { ListRow } from "@/components/list-row";
import {
  FormOutcome,
  SecretInput,
  SelectField,
  SettingField,
  SwitchField,
} from "@/components/settings";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Field, FieldDescription, FieldLabel, FieldLegend, FieldSet } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage, queryClient } from "@/lib/api";
import {
  type BackendForm,
  MAKE_DRIVES,
  backendChanges,
  backendDetail,
  backendForm,
  backendState,
  backupNote,
  choiceOptions,
  formProblem,
  listEveryOptions,
  newBackendBody,
  storedOn,
} from "@/lib/backends";
import { byName } from "@/lib/drives";
import { sealBackendSecretForMe } from "@/lib/encryption";
import { formatUsage } from "@/lib/users";

/** Everything that shows backends or where drives are stored, fetched again. */
export function refreshBackends() {
  void queryClient.invalidateQueries();
}

/** A backend's status badge: Connected, Unreachable, Locked (or Unlocked). */
export function BackendBadge({ backend }: { backend: Pick<Backend, "status" | "private"> }) {
  const state = backendState(backend);
  return (
    <Badge
      variant={
        state.tone === "error" ? "destructive" : state.tone === "locked" ? "outline" : "secondary"
      }
    >
      {state.tone === "locked" && <LockIcon />}
      {state.label}
    </Badge>
  );
}

/**
 * A backend in a list: its name and status, where it is and its drives,
 * whether the server backs it up, its usage, why it fails, and its menu.
 */
export function BackendRow({ backend, menu }: { backend: Backend; menu?: React.ReactNode }) {
  const disk = backend.kind === "disk";
  return (
    <div className="py-1">
      <ListRow
        icon={
          disk ? (
            <HardDriveIcon className="size-4 shrink-0 text-muted-foreground" />
          ) : (
            <CloudIcon className="size-4 shrink-0 text-muted-foreground" />
          )
        }
        title={
          <span className="flex items-center gap-2">
            <span className="truncate">{backend.name}</span>
            {!disk && <BackendBadge backend={backend} />}
          </span>
        }
        detail={backendDetail(backend)}
      >
        <span className="hidden shrink-0 text-sm text-muted-foreground sm:inline">
          {formatUsage(backend.used, backend.limit)}
        </span>
        {menu}
      </ListRow>
      {backend.error && backend.status !== "ok" && (
        <p className="pb-1 pl-6 text-sm text-destructive">{backend.error}</p>
      )}
    </div>
  );
}

/** What a connection test found, for a toast or the form. */
export function testedText(result: BackendTest): string | undefined {
  return result.ok ? "Connected: a test object was written, read back and deleted." : undefined;
}

/**
 * "Add S3 storage", or editing one: the connection, its secret (never
 * shown back), who makes drives on it (a global one), its limits, how
 * often it is looked at for changes, and private credentials (a user's
 * own). "Test connection" tries the unsaved settings.
 */
export function BackendDialog({
  backend,
  own,
  me,
  onClose,
}: {
  backend?: Backend;
  /** A user's own storage (Settings → Storage → Your storage). */
  own: boolean;
  /** The user's id: a private secret is bound to it. */
  me: string;
  onClose: () => void;
}) {
  const [form, setForm] = useState<BackendForm>(backendForm(backend));
  const [problem, setProblem] = useState<string>();
  const [tested, setTested] = useState<string>();
  const [testFailed, setTestFailed] = useState<string>();
  const [sealing, setSealing] = useState(false);
  const set = (changes: Partial<BackendForm>) => setForm({ ...form, ...changes });
  const teams = useQuery({ ...listTeamsOptions(), enabled: !own });
  const keys = useQuery({ ...getEncryptionOptions(), enabled: own });
  const encryptionSetUp = keys.data?.set_up ?? false;
  const saved = () => {
    refreshBackends();
    toast.success(backend ? "Saved" : "Storage added");
    onClose();
  };
  const addGlobal = useMutation({ ...addGlobalBackendMutation(), onSuccess: saved });
  const changeGlobal = useMutation({ ...changeGlobalBackendMutation(), onSuccess: saved });
  const addOwn = useMutation({ ...addBackendMutation(), onSuccess: saved });
  const changeOwn = useMutation({ ...changeBackendMutation(), onSuccess: saved });
  const save = own ? (backend ? changeOwn : addOwn) : backend ? changeGlobal : addGlobal;
  const onTested = (result: BackendTest) => {
    setTested(testedText(result));
    setTestFailed(result.ok ? undefined : (result.error ?? "The connection failed"));
  };
  const testOwnSettings = useMutation({ ...testBackendSettingsMutation(), onSuccess: onTested });
  const testGlobalSettings = useMutation({
    ...testGlobalBackendSettingsMutation(),
    onSuccess: onTested,
  });
  const testOwnSaved = useMutation({ ...testBackendMutation(), onSuccess: onTested });
  const testGlobalSaved = useMutation({ ...testGlobalBackendMutation(), onSuccess: onTested });
  const tests = [testOwnSettings, testGlobalSettings, testOwnSaved, testGlobalSaved];
  const testing = tests.some((t) => t.isPending);
  const testError = tests.find((t) => t.isError)?.error ?? testFailed;

  const test = () => {
    save.reset();
    for (const t of tests) t.reset();
    setTested(undefined);
    setTestFailed(undefined);
    // A saved backend whose secret is not typed again is tested as stored.
    if (backend && !form.secret) {
      const path = { id: backend.id };
      if (own) testOwnSaved.mutate({ path });
      else testGlobalSaved.mutate({ path });
      return;
    }
    const wrong = formProblem(form, true);
    setProblem(wrong);
    if (wrong) return;
    const body = newBackendBody(form, own);
    if (own) testOwnSettings.mutate({ body });
    else testGlobalSettings.mutate({ body });
  };

  const submit = async () => {
    // A test's outcome gives way to the save's.
    for (const t of tests) t.reset();
    setTested(undefined);
    setTestFailed(undefined);
    const wrong = formProblem(form, !backend);
    setProblem(wrong);
    if (wrong) return;
    let sealed: string | undefined;
    if (own && form.private && form.secret) {
      setSealing(true);
      try {
        sealed = await sealBackendSecretForMe(form.secret, me);
      } catch (error) {
        setProblem(errorMessage(error));
        return;
      } finally {
        setSealing(false);
      }
    }
    if (backend) {
      const body = {
        ...backendChanges(form, backend, own),
        ...(sealed ? { sealed_secret: sealed } : {}),
      };
      if (own) changeOwn.mutate({ path: { id: backend.id }, body });
      else changeGlobal.mutate({ path: { id: backend.id }, body });
    } else {
      const body = { ...newBackendBody(form, own), ...(sealed ? { sealed_secret: sealed } : {}) };
      if (own) addOwn.mutate({ body });
      else addGlobal.mutate({ body });
    }
  };

  return (
    <FormDialog
      title={backend ? `Edit ${backend.name}` : "Add S3 storage"}
      description="Any S3-compatible storage: AWS, Backblaze B2, Cloudflare R2, MinIO, Garage, Scaleway. The connection is tested before it is saved."
      action={backend ? "Save" : "Add"}
      pending={save.isPending || sealing}
      error={problem}
      onSubmit={() => void submit()}
      onClose={onClose}
      extra={
        <Button type="button" variant="outline" disabled={testing} onClick={test}>
          {testing ? "Testing…" : "Test connection"}
        </Button>
      }
    >
      <SettingField id="backend-name" label="Name">
        <Input
          id="backend-name"
          placeholder="Backblaze"
          value={form.name}
          onChange={(e) => set({ name: e.target.value })}
          autoFocus
        />
      </SettingField>
      <SettingField
        id="backend-endpoint"
        label="Endpoint"
        description={own ? "https, at a public address." : undefined}
      >
        <Input
          id="backend-endpoint"
          placeholder="https://s3.eu-central-1.amazonaws.com"
          value={form.endpoint}
          onChange={(e) => set({ endpoint: e.target.value })}
        />
      </SettingField>
      <SettingField id="backend-region" label="Region" description="Empty for us-east-1.">
        <Input
          id="backend-region"
          placeholder="us-east-1"
          value={form.region}
          onChange={(e) => set({ region: e.target.value })}
        />
      </SettingField>
      <SettingField
        id="backend-bucket"
        label="Bucket / prefix"
        description={
          backend && backend.drives > 0
            ? "Its drives are there: it stays while it holds any."
            : "Each drive gets a folder of its own under the prefix."
        }
      >
        <Input
          id="backend-bucket"
          placeholder="my-bucket/organizer"
          value={form.bucket}
          disabled={!!backend && backend.drives > 0}
          onChange={(e) => set({ bucket: e.target.value })}
        />
      </SettingField>
      <SettingField id="backend-access-key" label="Access key">
        <Input
          id="backend-access-key"
          autoComplete="off"
          value={form.accessKey}
          onChange={(e) => set({ accessKey: e.target.value })}
        />
      </SettingField>
      <SettingField
        id="backend-secret"
        label="Secret"
        description={
          backend ? "Never shown again. Type a new one to replace it." : "Never shown again."
        }
      >
        <SecretInput
          id="backend-secret"
          set={backend?.secret_set ?? false}
          value={form.secret}
          onChange={(secret) => set({ secret })}
        />
      </SettingField>
      {!own && (
        <SelectField
          id="backend-make-drives"
          label="Who makes drives"
          items={MAKE_DRIVES}
          value={form.makeDrives}
          onChange={(value) => set({ makeDrives: value as BackendForm["makeDrives"] })}
        />
      )}
      {!own && form.makeDrives === "teams" && (
        <FieldSet>
          <FieldLegend variant="label">Teams whose managers make drives</FieldLegend>
          {teams.data?.length === 0 && (
            <FieldDescription>No teams yet: make one in Settings → Teams.</FieldDescription>
          )}
          {byName(teams.data ?? [], (t) => t.name).map((team) => (
            <Field key={team.id} orientation="horizontal">
              <Checkbox
                id={`backend-team-${team.id}`}
                checked={form.teamIds.includes(team.id)}
                onCheckedChange={(on) =>
                  set({
                    teamIds: on
                      ? [...form.teamIds, team.id]
                      : form.teamIds.filter((id) => id !== team.id),
                  })
                }
              />
              <FieldLabel htmlFor={`backend-team-${team.id}`}>{team.name}</FieldLabel>
            </Field>
          ))}
        </FieldSet>
      )}
      <SettingField
        id="backend-limit"
        label="Storage limit in GB"
        description="For all its drives together. Empty for no limit."
      >
        <Input
          id="backend-limit"
          inputMode="decimal"
          placeholder="No limit"
          value={form.limit}
          onChange={(e) => set({ limit: e.target.value })}
        />
      </SettingField>
      <SettingField
        id="backend-drive-limit"
        label="Default drive limit in GB"
        description="Each new drive's limit. Empty for none."
      >
        <Input
          id="backend-drive-limit"
          inputMode="decimal"
          placeholder="No limit"
          value={form.driveLimit}
          onChange={(e) => set({ driveLimit: e.target.value })}
        />
      </SettingField>
      <SelectField
        id="backend-list-every"
        label="Look for changes"
        description="How often Organizer lists its drives for files changed by other tools."
        items={listEveryOptions(form.listEvery)}
        value={form.listEvery}
        onChange={(listEvery) => set({ listEvery })}
      />
      {own && (
        <>
          <SwitchField
            id="backend-private"
            label="Private credentials"
            description="The secret is sealed with your encryption key, so the server can't read it. Your drives here are only available while you're signed in with encryption unlocked."
            checked={form.private}
            disabled={!!backend || !encryptionSetUp}
            onChange={(on) => set({ private: on })}
          />
          {keys.data && !encryptionSetUp && !backend && (
            <FieldDescription>
              Private credentials need encryption:{" "}
              <Link to="/settings/encryption" className="underline underline-offset-4">
                set it up in Settings → Encryption
              </Link>{" "}
              first.
            </FieldDescription>
          )}
        </>
      )}
      <FormOutcome
        tested={tested}
        testError={testError}
        saveError={save.isError ? save.error : undefined}
      />
    </FormDialog>
  );
}

/**
 * "Stored on": where a new drive goes (the server's disk first), with the
 * user's own storage marked, and a note when the server's backups do not
 * cover the choice.
 */
export function StoredOnField({
  choices,
  value,
  onChange,
}: {
  choices: BackendChoice[] | undefined;
  value: string;
  onChange: (id: string) => void;
}) {
  const chosen = choices?.find((c) => c.id === value);
  return (
    <SelectField
      id="drive-stored-on"
      placeholder="Choose where"
      label="Stored on"
      items={choiceOptions(choices)}
      value={value}
      onChange={onChange}
      description={backupNote(chosen)}
    />
  );
}

/** The global backends as the superadmin's choices (they may use any). */
export function globalChoices(backends: Backend[] | undefined): BackendChoice[] | undefined {
  return backends?.map((b) => ({
    id: b.id,
    kind: b.kind,
    name: b.name,
    owner: null,
    backed_up: b.backed_up,
    available: b.status === "ok",
    error: b.error,
  }));
}

/**
 * "Move to other storage…": where to, then Move. A job copies the drive;
 * it is read-only meanwhile.
 */
export function MoveDriveDialog({
  drive,
  choices,
  pending,
  error,
  onMove,
  onClose,
}: {
  drive: Pick<Drive, "name" | "backend">;
  choices: BackendChoice[] | undefined;
  pending: boolean;
  error: unknown;
  onMove: (backendId: string) => void;
  onClose: () => void;
}) {
  const targets = choices?.filter((c) => c.id !== drive.backend.id);
  const [to, setTo] = useState("");
  const [problem, setProblem] = useState<string>();
  return (
    <FormDialog
      title={`Move ${drive.name} to other storage`}
      description={`${storedOn(drive)} now. Its files, trash and versions are copied, then it switches over; until then it is read-only for everyone.`}
      action={pending ? "Moving…" : "Move"}
      pending={pending}
      error={problem ?? error}
      onSubmit={() => {
        setProblem(to ? undefined : "Choose where to move it");
        if (to) onMove(to);
      }}
      onClose={onClose}
    >
      {targets?.length === 0 ? (
        <p className="text-sm text-muted-foreground">There is no other storage you may use.</p>
      ) : (
        <StoredOnField choices={targets} value={to} onChange={setTo} />
      )}
    </FormDialog>
  );
}
