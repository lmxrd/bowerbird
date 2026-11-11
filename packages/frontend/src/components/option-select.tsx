import type { Role } from "organizer-sdk";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { ROLE_LABELS } from "@/lib/roles";

/**
 * A small select beside a row or a setting: a role, a level's mode. Its
 * value is one of `options`.
 */
export function OptionSelect<T extends string>({
  options,
  value,
  onChange,
  disabled,
  label,
  className,
}: {
  options: readonly { value: T; label: string }[];
  value: T | undefined;
  onChange: (value: T) => void;
  disabled?: boolean;
  /** What it chooses, for screen readers: "Role of anna@example.com". */
  label: string;
  className?: string;
}) {
  return (
    <Select
      items={options.map((o) => ({ value: o.value, label: o.label }))}
      value={value ?? null}
      disabled={disabled}
      onValueChange={(next) => next && onChange(next as T)}
    >
      <SelectTrigger size="sm" aria-label={label} className={className}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="end" alignItemWithTrigger={false}>
        {options.map((o) => (
          <SelectItem key={o.value} value={o.value}>
            {o.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/** A role to give, of `roles` (the highest first). */
export function RoleSelect({
  roles,
  value,
  onChange,
  disabled,
  label,
}: {
  roles: readonly Role[];
  value: Role | undefined;
  onChange: (role: Role) => void;
  disabled?: boolean;
  label: string;
}) {
  // A role one may not give is still shown as it is.
  const shown = value && !roles.includes(value) ? [value, ...roles] : roles;
  return (
    <OptionSelect
      options={shown.map((role) => ({ value: role, label: ROLE_LABELS[role] }))}
      value={value}
      onChange={onChange}
      disabled={disabled}
      label={label}
      className="w-32"
    />
  );
}
