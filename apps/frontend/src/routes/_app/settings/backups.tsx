import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { TriangleAlertIcon } from "lucide-react";
import { toast } from "sonner";
import type { BackupSettings, UpdateBackupSettings } from "organizer-sdk";
import {
  getBackupSettingsOptions,
  runBackupMutation,
  testBackupSettingsMutation,
  updateBackupSettingsMutation,
} from "organizer-sdk/react-query";
import {
  changedFields,
  FormOutcome,
  SecretInput,
  SettingField,
  SettingsCard,
} from "@/components/settings";
import { Stat } from "@/components/stat";
import { SuperadminGate } from "@/components/step-up";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { FieldDescription, FieldGroup } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { errorMessage, queryClient } from "@/lib/api";
import { formatSchedule, generatePassword, parseSchedule, type Schedule } from "@/lib/backup";
import { formatDateTime, formatSize } from "@/lib/format";

// A superadmin page: a step-up first (`SuperadminGate`).
export const Route = createFileRoute("/_app/settings/backups")({
  component: () => (
    <SuperadminGate>
      <Backups />
    </SuperadminGate>
  ),
});

const texts = ["repository", "s3_endpoint", "s3_region", "s3_access_key"] as const;
const keeps = [
  { key: "keep_daily", unit: "daily" },
  { key: "keep_weekly", unit: "weekly" },
  { key: "keep_monthly", unit: "monthly" },
] as const;
type Key = (typeof texts)[number] | (typeof keeps)[number]["key"];
type Form = Record<Key, string>;

/** The variable that sets a field. */
const variable = (key: string) =>
  key.startsWith("keep_") ? "BACKUP_KEEP" : `BACKUP_${key.toUpperCase()}`;

function formOf(data: BackupSettings): Form {
  return {
    repository: data.repository,
    s3_endpoint: data.s3_endpoint,
    s3_region: data.s3_region,
    s3_access_key: data.s3_access_key,
    keep_daily: String(data.keep_daily),
    keep_weekly: String(data.keep_weekly),
    keep_monthly: String(data.keep_monthly),
  };
}

function Backups() {
  const settings = useQuery({
    ...getBackupSettingsOptions(),
    // Followed while a backup runs.
    refetchInterval: (query) => (query.state.data?.running ? 3000 : false),
  });
  const data = settings.data;
  const [form, setForm] = useState<Form>();
  const [schedule, setSchedule] = useState<Schedule>({ kind: "daily", time: "03:00" });
  // Undefined: kept as they are.
  const [secretKey, setSecretKey] = useState<string>();
  const [password, setPassword] = useState<string>();
  const [generated, setGenerated] = useState(false);
  const [tested, setTested] = useState<string>();
  useEffect(() => {
    if (data) {
      setForm(formOf(data));
      setSchedule(parseSchedule(data.schedule));
    }
  }, [data]);
  const save = useMutation({
    ...updateBackupSettingsMutation(),
    onSuccess: () => {
      setSecretKey(undefined);
      setPassword(undefined);
      setGenerated(false);
      void queryClient.invalidateQueries();
      toast.success("Saved");
    },
  });
  const test = useMutation({
    ...testBackupSettingsMutation(),
    onMutate: () => setTested(undefined),
    onSuccess: (result) =>
      setTested(
        result.exists
          ? `The repository opens: ${result.snapshots} backups.`
          : "Reachable. The first backup makes the repository.",
      ),
  });
  const run = useMutation({
    ...runBackupMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Backing up");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const locked = (key: string) => data?.locked.includes(key) ?? false;
  const lockedBy = (key: string) => (locked(key) ? variable(key) : undefined);

  const changes = (): UpdateBackupSettings => {
    if (!data || !form) return {};
    const changed = changedFields(form, formOf(data), locked);
    const body: UpdateBackupSettings = {};
    for (const key of texts) if (changed[key] !== undefined) body[key] = changed[key].trim();
    for (const { key } of keeps) if (changed[key] !== undefined) body[key] = Number(changed[key]);
    const text = formatSchedule(schedule);
    if (text !== data.schedule && !locked("schedule")) body.schedule = text;
    if (secretKey !== undefined && !locked("s3_secret_key")) body.s3_secret_key = secretKey;
    if (password !== undefined && !locked("password")) body.password = password;
    return body;
  };

  const s3 = form?.repository.startsWith("s3://") ?? false;
  const textField = (key: (typeof texts)[number], label: string, placeholder: string) =>
    form && (
      <SettingField id={key} label={label} lockedBy={lockedBy(key)}>
        <Input
          id={key}
          placeholder={placeholder}
          value={form[key]}
          disabled={locked(key)}
          onChange={(e) => setForm({ ...form, [key]: e.target.value })}
        />
      </SettingField>
    );

  return (
    <SettingsCard
      title="Backups"
      description="The whole server: the database, every user's files, the trash, versions and drop box. Encrypted, compressed and deduplicated (restic format), on another disk or S3."
      enabled={data?.enabled}
      error={settings.error}
    >
      {data && form && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            test.reset();
            setTested(undefined);
            save.mutate({ body: changes() });
          }}
        >
          <FieldGroup>
            <BackupStatus data={data} />
            {textField("repository", "Repository", "/mnt/backup/organizer or s3://bucket/path")}
            {s3 && (
              <>
                <div className="grid gap-4 sm:grid-cols-2">
                  {textField("s3_endpoint", "S3 endpoint", "Empty for AWS")}
                  {textField("s3_region", "Region", "us-east-1")}
                </div>
                <div className="grid gap-4 sm:grid-cols-2">
                  {textField("s3_access_key", "Access key", "")}
                  <SettingField
                    id="s3_secret_key"
                    label="Secret key"
                    lockedBy={lockedBy("s3_secret_key")}
                  >
                    <SecretInput
                      id="s3_secret_key"
                      set={data.s3_secret_key_set}
                      value={secretKey ?? ""}
                      disabled={locked("s3_secret_key")}
                      onChange={setSecretKey}
                    />
                  </SettingField>
                </div>
              </>
            )}
            <SettingField id="password" label="Password" lockedBy={lockedBy("password")}>
              <div className="flex gap-2">
                {generated ? (
                  <Input id="password" readOnly value={password ?? ""} className="font-mono" />
                ) : (
                  <SecretInput
                    id="password"
                    set={data.password_set}
                    value={password ?? ""}
                    disabled={locked("password")}
                    onChange={setPassword}
                  />
                )}
                {!locked("password") && (
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => {
                      setPassword(generatePassword());
                      setGenerated(true);
                    }}
                  >
                    Generate
                  </Button>
                )}
              </div>
            </SettingField>
            <Alert>
              <TriangleAlertIcon />
              <AlertDescription>
                {generated
                  ? "Copy this password and keep it somewhere other than this server: it is not shown again, and without it the backups can't be read."
                  : "Without this password the backups can't be read. Keep a copy somewhere other than this server. Changing it here does not change an existing repository's."}
              </AlertDescription>
            </Alert>
            <SettingField label="Schedule" lockedBy={lockedBy("schedule")}>
              <div className="flex gap-2">
                <Select
                  items={[
                    { value: "daily", label: "Daily at" },
                    { value: "every", label: "Every … hours" },
                  ]}
                  value={schedule.kind}
                  disabled={locked("schedule")}
                  onValueChange={(kind) =>
                    setSchedule(
                      kind === "every" ? { kind, hours: "6" } : { kind: "daily", time: "03:00" },
                    )
                  }
                >
                  <SelectTrigger className="w-40">
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value="daily">Daily at</SelectItem>
                    <SelectItem value="every">Every … hours</SelectItem>
                  </SelectContent>
                </Select>
                {schedule.kind === "daily" ? (
                  <Input
                    type="time"
                    aria-label="Time"
                    className="w-32"
                    value={schedule.time}
                    disabled={locked("schedule")}
                    onChange={(e) => setSchedule({ kind: "daily", time: e.target.value })}
                  />
                ) : (
                  <Input
                    type="number"
                    aria-label="Hours"
                    min={1}
                    className="w-24"
                    value={schedule.hours}
                    disabled={locked("schedule")}
                    onChange={(e) => setSchedule({ kind: "every", hours: e.target.value })}
                  />
                )}
              </div>
            </SettingField>
            <SettingField
              label="Keep"
              lockedBy={lockedBy("keep_daily")}
              description="The last backup of so many days, weeks and months; the newest always."
            >
              <div className="flex flex-wrap items-center gap-2 text-sm">
                {keeps.map(({ key, unit }) => (
                  <span key={key} className="flex items-center gap-2">
                    <Input
                      type="number"
                      min={0}
                      aria-label={`Keep ${unit}`}
                      className="w-20"
                      value={form[key]}
                      disabled={locked(key)}
                      onChange={(e) => setForm({ ...form, [key]: e.target.value })}
                    />
                    {unit}
                  </span>
                ))}
              </div>
            </SettingField>
            <FieldDescription>
              Each user can restore their own files from a backup (a folder's menu → From a backup).
              Restore everything with <code>organizer backup restore</code>, the server stopped.
            </FieldDescription>
            <FormOutcome
              tested={tested}
              testError={test.isError ? test.error : undefined}
              saveError={save.isError ? save.error : undefined}
            />
            <div className="flex flex-wrap justify-end gap-2">
              <Button
                type="button"
                variant="outline"
                disabled={test.isPending}
                onClick={() => {
                  save.reset();
                  test.mutate({ body: changes() });
                }}
              >
                {test.isPending ? "Testing…" : "Test"}
              </Button>
              <Button
                type="button"
                variant="outline"
                disabled={!data.enabled || data.running || run.isPending}
                onClick={() => run.mutate({})}
              >
                {data.running ? "Backing up…" : "Back up now"}
              </Button>
              <Button type="submit" disabled={save.isPending}>
                Save
              </Button>
            </div>
          </FieldGroup>
        </form>
      )}
    </SettingsCard>
  );
}

/** The last backup, the next one, and the repository. */
function BackupStatus({ data }: { data: BackupSettings }) {
  const status = data.status;
  if (!data.enabled && !status.last_started) return null;
  const failed = !!status.last_error;
  return (
    <>
      <div className="grid gap-3 sm:grid-cols-3">
        <Stat label="Last backup">
          {status.last_finished ? formatDateTime(status.last_finished) : "Never"}
          {status.last_finished && (
            <Badge variant={failed ? "destructive" : "secondary"}>{failed ? "Failed" : "OK"}</Badge>
          )}
        </Stat>
        <Stat label="Next">
          {data.running ? "Running now" : data.next_run ? formatDateTime(data.next_run) : "—"}
        </Stat>
        <Stat label="Repository">
          {status.snapshots != null
            ? `${status.snapshots} backups${status.last_total != null ? ` · ${formatSize(status.last_total)}` : ""}`
            : "—"}
        </Stat>
      </div>
      {failed && (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertDescription>{status.last_error}</AlertDescription>
        </Alert>
      )}
    </>
  );
}
