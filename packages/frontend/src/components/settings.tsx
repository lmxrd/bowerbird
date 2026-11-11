import { CircleCheckIcon, LockIcon } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import {
  Field,
  FieldContent,
  FieldDescription,
  FieldError,
  FieldLabel,
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
import { errorMessage } from "@/lib/api";

/** A settings section: a card with its title, what it is for, and whether it is on. */
export function SettingsCard({
  title,
  description,
  enabled,
  action,
  error,
  children,
}: {
  title: string;
  description?: React.ReactNode;
  /** Shown as an On/Off badge when given. */
  enabled?: boolean;
  /** A button in the card's corner ("Invite someone", "Add a model"), beside the badge if there is one. */
  action?: React.ReactNode;
  /** The settings could not be read. */
  error?: unknown;
  children: React.ReactNode;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        {description && <CardDescription>{description}</CardDescription>}
        {(enabled !== undefined || action) && (
          <CardAction className="flex items-center gap-2">
            {enabled !== undefined && (
              <Badge variant={enabled ? "default" : "outline"}>{enabled ? "On" : "Off"}</Badge>
            )}
            {action}
          </CardAction>
        )}
      </CardHeader>
      <CardContent>
        {error ? <p className="text-sm text-destructive">{errorMessage(error)}</p> : children}
      </CardContent>
    </Card>
  );
}

/**
 * A setting's field: its label, with a lock when an environment variable
 * sets it (and then which one), and its description otherwise.
 */
export function SettingField({
  id,
  label,
  lockedBy,
  description,
  children,
}: {
  id?: string;
  label: string;
  /** The variable that sets it; the field is locked when given. */
  lockedBy?: string;
  description?: React.ReactNode;
  /** The control, disabled by its page when locked. */
  children: React.ReactNode;
}) {
  return (
    <Field>
      <FieldLabel htmlFor={id}>
        {label}
        {lockedBy && <LockIcon className="size-3" />}
      </FieldLabel>
      {children}
      {lockedBy ? (
        <FieldDescription>
          <LockIcon className="inline size-3" /> Set by {lockedBy}
        </FieldDescription>
      ) : (
        description && <FieldDescription>{description}</FieldDescription>
      )}
    </Field>
  );
}

/**
 * A secret that is never sent back: empty means "keep it", and the
 * placeholder says whether one is set.
 */
export function SecretInput({
  id,
  set,
  value,
  disabled,
  onChange,
}: {
  id: string;
  /** One is stored. */
  set: boolean;
  value: string;
  disabled?: boolean;
  onChange: (value: string) => void;
}) {
  return (
    <Input
      id={id}
      type="password"
      autoComplete="off"
      placeholder={set ? "Set: type to replace it" : "Not set"}
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

/** What came of a test (or why it failed) and of a save that failed. */
export function FormOutcome({
  tested,
  testError,
  saveError,
}: {
  tested?: string;
  testError?: unknown;
  saveError?: unknown;
}) {
  return (
    <>
      {testError ? <FieldError>{errorMessage(testError)}</FieldError> : null}
      {tested && (
        <p className="flex items-center gap-1 text-sm text-muted-foreground">
          <CircleCheckIcon className="size-4" />
          {tested}
        </p>
      )}
      {saveError ? <FieldError>{errorMessage(saveError)}</FieldError> : null}
    </>
  );
}

/** The fields of a form that differ from what was loaded, without the locked ones. */
export function changedFields<K extends string, V>(
  form: Record<K, V>,
  loaded: Record<K, V>,
  locked: (key: K) => boolean,
): Partial<Record<K, V>> {
  const out: Partial<Record<K, V>> = {};
  for (const key of Object.keys(form) as K[]) {
    if (form[key] !== loaded[key] && !locked(key)) out[key] = form[key];
  }
  return out;
}

/**
 * A setting that is on or off: its switch, label and description; locked
 * (and saying by which variable) when an environment variable sets it.
 */
export function SwitchField({
  id,
  label,
  description,
  lockedBy,
  checked,
  disabled,
  onChange,
}: {
  id: string;
  label: string;
  description?: React.ReactNode;
  /** The variable that sets it; the switch is locked when given. */
  lockedBy?: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (checked: boolean) => void;
}) {
  return (
    <Field orientation="horizontal">
      <FieldContent>
        <FieldLabel htmlFor={id}>
          {label}
          {lockedBy && <LockIcon className="size-3" />}
        </FieldLabel>
        {description && <FieldDescription>{description}</FieldDescription>}
        {lockedBy && (
          <FieldDescription>
            <LockIcon className="inline size-3" /> Set by {lockedBy}
          </FieldDescription>
        )}
      </FieldContent>
      <Switch
        id={id}
        checked={checked}
        disabled={disabled || !!lockedBy}
        onCheckedChange={(value) => onChange(value)}
      />
    </Field>
  );
}

/** A setting chosen from a list: its select, with the label and lock of `SettingField`. */
export function SelectField({
  id,
  label,
  description,
  lockedBy,
  placeholder,
  items,
  value,
  onChange,
}: {
  id?: string;
  label: string;
  description?: React.ReactNode;
  /** The variable that sets it; the select is disabled when given. */
  lockedBy?: string;
  placeholder?: string;
  items: readonly { value: string; label: string; disabled?: boolean }[];
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <SettingField id={id} label={label} lockedBy={lockedBy} description={description}>
      {/* An empty value is Base UI's null: the item of "the default" or "off". */}
      <Select
        items={items.map((item) => ({ value: item.value || null, label: item.label }))}
        value={value || null}
        disabled={lockedBy !== undefined}
        onValueChange={(next) => onChange(next ?? "")}
      >
        <SelectTrigger id={id} className="w-full">
          <SelectValue placeholder={placeholder} />
        </SelectTrigger>
        <SelectContent>
          {items.map((item) => (
            <SelectItem key={item.value} value={item.value || null} disabled={item.disabled}>
              {item.label}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </SettingField>
  );
}
