import { useEffect, useState } from "react";
import type { Retention, RetentionChanges, RetentionRules, UserSetting } from "organizer-sdk";
import { FormOutcome, SettingField } from "@/components/settings";
import { Button } from "@/components/ui/button";
import { FieldDescription, FieldGroup } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { formatSize } from "@/lib/format";
import { lockedByName, userNote } from "@/lib/levels";

type Key = keyof RetentionRules;
type Form = Record<Key, string>;

const keys: Key[] = ["trash_days", "versions_hours", "versions_days", "versions_months"];
const units: Record<Key, string> = {
  trash_days: "days",
  versions_hours: "hours",
  versions_days: "days",
  versions_months: "months",
};

/** The form of loaded rules: the user's own where they differ from the defaults, else empty. */
function formOf(data: Retention): Form {
  return Object.fromEntries(
    keys.map((k) => [k, data.rules[k] !== data.defaults[k] ? String(data.rules[k]) : ""]),
  ) as Form;
}

/**
 * How long the trash and the versions are kept: a user's own rules (an
 * empty field is the default). Rules set by a variable or locked by a
 * level above (`levels`, the user's settings by key) are disabled, saying
 * who set them; the others say the range and whose default they follow.
 */
export function RetentionForm({
  data,
  levels,
  saving,
  saveError,
  onSave,
}: {
  data: Retention;
  levels: Map<string, UserSetting>;
  saving: boolean;
  saveError?: unknown;
  onSave: (changes: RetentionChanges) => void;
}) {
  const [form, setForm] = useState<Form>(() => formOf(data));
  useEffect(() => setForm(formOf(data)), [data]);
  const level = (key: Key) => levels.get(`retention.${key}`);
  const lockedBy = (key: Key) => {
    if (data.locked.includes(key)) return key.toUpperCase();
    const setting = level(key);
    return setting && !setting.may_change
      ? (lockedByName(setting) ?? "your administrator")
      : undefined;
  };
  const locked = (key: Key) => lockedBy(key) !== undefined;

  const number = (key: Key, label: string, unit: string) => {
    const setting = level(key);
    return (
      <SettingField
        id={key}
        label={label}
        lockedBy={lockedBy(key)}
        description={setting && userNote(setting, units[key])}
      >
        <div className="flex items-center gap-2">
          <Input
            id={key}
            type="number"
            min={setting?.min ?? 0}
            max={setting?.max ?? undefined}
            className="w-24"
            placeholder={String(data.defaults[key])}
            value={locked(key) ? String(data.rules[key]) : form[key]}
            disabled={locked(key)}
            onChange={(e) => setForm({ ...form, [key]: e.target.value })}
          />
          <span className="text-sm text-muted-foreground">{unit}</span>
        </div>
      </SettingField>
    );
  };

  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        const changes: RetentionChanges = {};
        for (const key of keys) {
          if (locked(key)) continue;
          const value = form[key].trim();
          changes[key] = value === "" ? null : Number(value);
        }
        onSave(changes);
      }}
    >
      <FieldGroup>
        {number("trash_days", "Empty the trash after", "days (0: never)")}
        <div className="grid gap-4 sm:grid-cols-3">
          {number("versions_hours", "Keep every version for", "hours")}
          {number("versions_days", "then one a day for", "days")}
          {number("versions_months", "then one a month for", "months")}
        </div>
        <FieldDescription>
          Versions are kept when a file is replaced through Organizer: the web app, the apps or
          WebDAV. Changes made directly in the files folder are only in the backups. All three at 0
          keeps no versions. An empty field follows the default, shown in grey.
        </FieldDescription>
        {data.versions_size != null && data.trash_size != null && (
          <FieldDescription>
            Space used: trash {formatSize(data.trash_size)}, versions{" "}
            {formatSize(data.versions_size)}.
          </FieldDescription>
        )}
        <FormOutcome saveError={saveError} />
        <Button type="submit" variant="outline" className="self-start" disabled={saving}>
          Save
        </Button>
      </FieldGroup>
    </form>
  );
}
