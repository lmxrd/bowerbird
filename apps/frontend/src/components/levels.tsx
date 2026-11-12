import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { LockIcon } from "lucide-react";
import { toast } from "sonner";
import type { LevelSetting, LevelValue, UserSetting } from "organizer-sdk";
import {
  listDriveSettingsOptions,
  listSelectableAiModelsOptions,
  listMySettingsOptions,
  listSuperadminSettingsOptions,
  listTeamSettingsOptions,
  setDriveSettingMutation,
  setMySettingMutation,
  setSuperadminSettingMutation,
  setTeamSettingMutation,
  unsetDriveSettingMutation,
  unsetMySettingMutation,
  unsetSuperadminSettingMutation,
  unsetTeamSettingMutation,
} from "organizer-sdk/react-query";
import { OptionSelect } from "@/components/option-select";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import {
  Field,
  FieldDescription,
  FieldGroup,
  FieldLabel,
  FieldLegend,
  FieldSet,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Switch } from "@/components/ui/switch";
import { modelChoicesFor, modelItems } from "@/lib/ai";
import { errorMessage, queryClient } from "@/lib/api";
import {
  GROUPS,
  type Json,
  type LevelChoice,
  amountText,
  byKey,
  choiceOf,
  infoOf,
  isNumber,
  jsonOf,
  levelChoices,
  levelWrite,
  lockedByName,
  parseAmount,
  rangeText,
  shownValue,
  toJson,
  userNote,
} from "@/lib/levels";

/**
 * The settings levels' controls (docs/M7B_DESIGN.md §4), one pair for
 * every page: `LevelSettingsCard` (with `LevelField` and its
 * `LevelControl`) for the superadmin's, a team's and a drive's settings,
 * and `LevelNote` / `MySettingField` for what a user sees of them.
 */

/** Whose settings a level page writes. */
export type LevelScope =
  | { level: "superadmin" }
  | { level: "team"; id: string }
  | { level: "drive"; id: string };

/** Every query of settings: a change at one level shows at the others. */
function refreshSettings() {
  for (const _id of [
    "listMySettings",
    "listSuperadminSettings",
    "listTeamSettings",
    "listDriveSettings",
    "getDropBoxSettings",
    "getRetention",
    "getNotifications",
    "getSession",
  ]) {
    void queryClient.invalidateQueries({ queryKey: [{ _id }] });
  }
}

const failed = (error: unknown) => toast.error(errorMessage(error));

/** A level's settings, and writing them. */
function useLevelSettings(scope: LevelScope) {
  const superadmin = useQuery({
    ...listSuperadminSettingsOptions(),
    enabled: scope.level === "superadmin",
  });
  const team = useQuery({
    ...listTeamSettingsOptions({
      path: { id: scope.level === "team" ? scope.id : "" },
    }),
    enabled: scope.level === "team",
  });
  const drive = useQuery({
    ...listDriveSettingsOptions({
      path: { id: scope.level === "drive" ? scope.id : "" },
    }),
    enabled: scope.level === "drive",
  });
  const done = { onSuccess: refreshSettings, onError: failed };
  const setSuperadmin = useMutation({ ...setSuperadminSettingMutation(), ...done });
  const unsetSuperadmin = useMutation({ ...unsetSuperadminSettingMutation(), ...done });
  const setTeam = useMutation({ ...setTeamSettingMutation(), ...done });
  const unsetTeam = useMutation({ ...unsetTeamSettingMutation(), ...done });
  const setDrive = useMutation({ ...setDriveSettingMutation(), ...done });
  const unsetDrive = useMutation({ ...unsetDriveSettingMutation(), ...done });
  const query = scope.level === "superadmin" ? superadmin : scope.level === "team" ? team : drive;
  const write = (key: string, value: LevelValue | null) => {
    if (scope.level === "superadmin") {
      if (value) setSuperadmin.mutate({ path: { key }, body: value });
      else unsetSuperadmin.mutate({ path: { key } });
    } else if (scope.level === "team") {
      const path = { id: scope.id, key };
      if (value) setTeam.mutate({ path, body: value });
      else unsetTeam.mutate({ path });
    } else {
      const path = { id: scope.id, key };
      if (value) setDrive.mutate({ path, body: value });
      else unsetDrive.mutate({ path });
    }
  };
  return { query, write };
}

/** The AI models one may choose for a model's setting (`ai.model.*`); none for others. */
function useModels(key: string) {
  const models = useQuery({
    ...listSelectableAiModelsOptions(),
    enabled: key.startsWith("ai.model."),
  });
  return modelChoicesFor(key, models.data);
}

/** A number field that saves when left or on Enter, in its unit. */
function AmountInput({
  id,
  kind,
  value,
  unit,
  disabled,
  placeholder,
  label,
  onCommit,
}: {
  id?: string;
  kind: string;
  value: number | null | undefined;
  unit?: string;
  disabled?: boolean;
  placeholder?: string;
  label?: string;
  onCommit: (value: number | null) => void;
}) {
  const [text, setText] = useState(amountText(value, kind));
  useEffect(() => setText(amountText(value, kind)), [value, kind]);
  const commit = () => {
    const parsed = parseAmount(text, kind);
    if (parsed === undefined) {
      toast.error("Expected a whole number");
      setText(amountText(value, kind));
    } else if (parsed !== (value ?? null)) {
      onCommit(parsed);
    }
  };
  return (
    <div className="flex items-center gap-2">
      <Input
        id={id}
        type="number"
        className="w-24"
        aria-label={label}
        value={text}
        placeholder={placeholder}
        disabled={disabled}
        onChange={(e) => setText(e.target.value)}
        onBlur={commit}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            commit();
          }
        }}
      />
      {(unit || kind === "fraction") && (
        <span className="text-sm text-muted-foreground">{kind === "fraction" ? "%" : unit}</span>
      )}
    </div>
  );
}

/**
 * A setting's value, edited as its kind asks: a switch, a number in its
 * unit, one of its choices, one of the models one may choose.
 */
function ValueEditor({
  id,
  settingKey,
  kind,
  choices,
  value,
  disabled,
  onChange,
}: {
  id: string;
  settingKey: string;
  kind: string;
  choices: string[];
  value: Json;
  disabled?: boolean;
  onChange: (value: Json) => void;
}) {
  const info = infoOf(settingKey);
  const models = useModels(settingKey);
  if (kind === "bool") {
    return (
      <Switch
        id={id}
        checked={value === true}
        disabled={disabled}
        onCheckedChange={(on) => onChange(on)}
      />
    );
  }
  if (isNumber(kind)) {
    return (
      <AmountInput
        id={id}
        kind={kind}
        unit={info.unit}
        value={typeof value === "number" ? value : null}
        disabled={disabled}
        onCommit={(n) => n !== null && onChange(n)}
      />
    );
  }
  if (kind === "choice" || kind === "model") {
    // A model: null is the AI page's choice.
    const items =
      kind === "choice"
        ? choices.map((c) => ({ value: c, label: info.choices?.[c] ?? c }))
        : modelItems(models, value);
    return (
      <Select
        items={items.map((i) => ({ value: i.value || null, label: i.label }))}
        value={typeof value === "string" && value ? value : null}
        disabled={disabled}
        onValueChange={(next) => onChange(next ?? null)}
      >
        <SelectTrigger id={id} className="w-full">
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {items.map((i) => (
            <SelectItem key={i.value} value={i.value || null}>
              {i.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    );
  }
  return <p className="text-sm text-muted-foreground">Each user sets this.</p>;
}

/**
 * The small select beside a setting on the superadmin's and team
 * managers' pages: Users may change / Locked / A range / Not set.
 */
export function LevelControl({
  setting,
  onChange,
}: {
  setting: LevelSetting;
  onChange: (choice: LevelChoice) => void;
}) {
  return (
    <OptionSelect
      options={levelChoices(setting.kind)}
      value={choiceOf(setting)}
      disabled={!!setting.above.locked_by}
      onChange={onChange}
      label={`Level of ${infoOf(setting.key).label}`}
      className="w-40"
    />
  );
}

/**
 * A setting on a level's page: its label with the level control, then its
 * value (or its range), and what the levels above leave it.
 */
export function LevelField({
  setting,
  onWrite,
}: {
  setting: LevelSetting;
  /** A new mode and value at this level; null unsets it. */
  onWrite: (value: LevelValue | null) => void;
}) {
  const info = infoOf(setting.key);
  const id = `level-${setting.key}`;
  const choice = choiceOf(setting);
  const above = lockedByName(setting.above);
  const aboveRange = rangeText(setting.above.min, setting.above.max, setting.kind, info.unit);
  // A value typed while not set gives it as a default.
  const writeValue = (value: Json) =>
    onWrite({
      mode: choice === "locked" ? "locked" : "default",
      value: toJson(value),
    });
  return (
    <Field>
      <div className="flex items-center justify-between gap-2">
        <FieldLabel htmlFor={id}>
          {info.label}
          {above && <LockIcon className="size-3" />}
        </FieldLabel>
        <LevelControl setting={setting} onChange={(c) => onWrite(levelWrite(setting, c))} />
      </div>
      {choice === "range" ? (
        <div className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
          At least
          <AmountInput
            kind={setting.kind}
            label="At least"
            placeholder="—"
            value={setting.min}
            onCommit={(min) => onWrite({ mode: "range", min, max: setting.max })}
          />
          at most
          <AmountInput
            kind={setting.kind}
            unit={infoOf(setting.key).unit}
            label="At most"
            placeholder="—"
            value={setting.max}
            onCommit={(max) => onWrite({ mode: "range", min: setting.min, max })}
          />
        </div>
      ) : (
        <ValueEditor
          id={id}
          settingKey={setting.key}
          kind={setting.kind}
          choices={setting.choices}
          value={shownValue(setting)}
          disabled={!!above}
          onChange={writeValue}
        />
      )}
      <FieldDescription>
        {above ? (
          <>
            <LockIcon className="inline size-3" /> Set by {above}
          </>
        ) : choice === "unset" ? (
          `Not set here: ${aboveRange ? `${aboveRange}, ` : ""}those below choose.`
        ) : choice === "locked" ? (
          "Locked: those below see it and can't change it."
        ) : choice === "range" ? (
          "Those below choose within it."
        ) : (
          `A default: those below may change it${aboveRange ? ` (${aboveRange})` : ""}.`
        )}
      </FieldDescription>
    </Field>
  );
}

/**
 * A level's settings as a card: those of `prefixes` (every one when not
 * given), grouped as the team defaults page groups them when there are
 * several groups.
 */
export function LevelSettingsCard({
  scope,
  prefixes,
  title,
  description,
}: {
  scope: LevelScope;
  /** The keys' prefixes to show ("retention."); all by default. */
  prefixes?: string[];
  title: string;
  description?: React.ReactNode;
}) {
  const { query, write } = useLevelSettings(scope);
  const all = query.data ?? [];
  const groups = GROUPS.map((g) => ({
    ...g,
    settings: all.filter(
      (s) =>
        s.key.startsWith(g.prefix) &&
        s.kind !== "folders" &&
        (!prefixes || prefixes.some((p) => s.key.startsWith(p))),
    ),
  })).filter((g) => g.settings.length > 0);
  const field = (s: LevelSetting) => (
    <LevelField key={s.key} setting={s} onWrite={(value) => write(s.key, value)} />
  );
  return (
    <SettingsCard title={title} description={description} error={query.error}>
      {groups.length === 1 ? (
        <FieldGroup>{groups[0].settings.map(field)}</FieldGroup>
      ) : (
        <FieldGroup>
          {groups.map((g) => (
            <FieldSet key={g.prefix}>
              <FieldLegend>{g.title}</FieldLegend>
              <FieldGroup>{g.settings.map(field)}</FieldGroup>
            </FieldSet>
          ))}
        </FieldGroup>
      )}
    </SettingsCard>
  );
}

/** The user's settings by key, as the levels leave them. */
export function useMySettings(): Map<string, UserSetting> {
  const mine = useQuery(listMySettingsOptions());
  return byKey(mine.data);
}

/**
 * What a user sees of a setting's levels, under its field: locked, "Set by
 * your administrator" or "Set by <team>"; else the range the levels above
 * leave and whose default it is.
 */
export function LevelNote({ setting }: { setting: UserSetting | undefined }) {
  if (!setting) return null;
  const by = lockedByName(setting);
  if (by) {
    return (
      <FieldDescription>
        <LockIcon className="inline size-3" /> Set by {by}
      </FieldDescription>
    );
  }
  const note = userNote(setting, infoOf(setting.key).unit);
  return note ? <FieldDescription>{note}</FieldDescription> : null;
}

/** Whether a user may not change a setting (a level above locks it, or it has no user level). */
export function lockedForMe(setting: UserSetting | undefined): boolean {
  return setting !== undefined && !setting.may_change;
}

/**
 * Who a setting the user may not change is set by, for a field's
 * `lockedBy` ("Set by your administrator", "Set by Staff"); none when they
 * may change it.
 */
export function lockedByOf(setting: UserSetting | undefined): string | undefined {
  if (!lockedForMe(setting)) return undefined;
  return (setting && lockedByName(setting)) ?? "your administrator";
}

/**
 * A user's setting as its kind asks, saved when changed, with its level
 * note and, for a value of their own, "Use the default".
 */
export function MySettingField({ setting }: { setting: UserSetting }) {
  const info = infoOf(setting.key);
  const id = `mine-${setting.key}`;
  const set = useMutation({
    ...setMySettingMutation(),
    onSuccess: refreshSettings,
    onError: failed,
  });
  const unset = useMutation({
    ...unsetMySettingMutation(),
    onSuccess: refreshSettings,
    onError: failed,
  });
  const path = { key: setting.key };
  return (
    <Field>
      <div className="flex items-center justify-between gap-2">
        <FieldLabel htmlFor={id}>
          {info.label}
          {setting.locked_by && <LockIcon className="size-3" />}
        </FieldLabel>
        {setting.source.level === "user" && setting.may_change && (
          <Button variant="ghost" size="sm" onClick={() => unset.mutate({ path })}>
            Use the default
          </Button>
        )}
      </div>
      <ValueEditor
        id={id}
        settingKey={setting.key}
        kind={setting.kind}
        choices={setting.choices}
        value={jsonOf(setting.value)}
        disabled={!setting.may_change}
        onChange={(value) => set.mutate({ path, body: { value: toJson(value) } })}
      />
      <LevelNote setting={setting} />
    </Field>
  );
}
