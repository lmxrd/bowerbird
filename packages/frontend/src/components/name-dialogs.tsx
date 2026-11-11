import { useState } from "react";
import { FormDialog } from "@/components/form-dialog";
import { SettingField } from "@/components/settings";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { limitToGb, parseLimitGb } from "@/lib/users";

/** "Rename": a drive's or a team's name, through `onSave`. */
export function RenameDialog({
  initial,
  pending,
  error,
  onSave,
  onClose,
}: {
  initial: string;
  pending: boolean;
  error: unknown;
  onSave: (name: string) => void;
  onClose: () => void;
}) {
  const [name, setName] = useState(initial);
  return (
    <FormDialog
      title={`Rename ${initial}`}
      action="Rename"
      pending={pending}
      error={error}
      onSubmit={() => onSave(name.trim())}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="new-name">Name</FieldLabel>
        <Input
          id="new-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          autoFocus
          required
        />
      </Field>
    </FormDialog>
  );
}

/** "Storage limit…" of a drive or a team: none, or so many GB, through `onSave`. */
export function StorageLimitDialog({
  description,
  limit,
  pending,
  error,
  onSave,
  onClose,
}: {
  description: string;
  /** Bytes; null for none. */
  limit: number | null;
  pending: boolean;
  error: unknown;
  onSave: (limit: number | null) => void;
  onClose: () => void;
}) {
  const [text, setText] = useState(limitToGb(limit));
  const [invalid, setInvalid] = useState(false);
  return (
    <FormDialog
      title="Storage limit"
      description={description}
      action="Save"
      pending={pending}
      error={invalid ? "Expected a number of GB, or nothing for no limit" : error}
      onSubmit={() => {
        const parsed = parseLimitGb(text);
        setInvalid(parsed === undefined);
        if (parsed !== undefined) onSave(parsed);
      }}
      onClose={onClose}
    >
      <SettingField id="limit" label="Limit in GB" description="Empty for no limit.">
        <Input
          id="limit"
          inputMode="decimal"
          placeholder="No limit"
          value={text}
          onChange={(e) => setText(e.target.value)}
          autoFocus
        />
      </SettingField>
    </FormDialog>
  );
}
