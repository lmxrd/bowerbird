import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/api";

/**
 * An email and a new password, and "Create account": the invite page and
 * open sign-up. The email is filled in and locked when it is given.
 */
export function NewAccountForm({
  email: fixedEmail,
  pending,
  error,
  onSubmit,
}: {
  /** The invite's email, which the account takes. */
  email?: string | null;
  pending: boolean;
  error?: unknown;
  onSubmit: (email: string, password: string) => void;
}) {
  const [email, setEmail] = useState(fixedEmail ?? "");
  const [password, setPassword] = useState("");
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        onSubmit(fixedEmail ?? email, password);
      }}
    >
      <FieldGroup>
        <Field>
          <FieldLabel htmlFor="email">Email</FieldLabel>
          <Input
            id="email"
            type="email"
            autoComplete="username"
            placeholder="name@example.com"
            value={fixedEmail ?? email}
            onChange={(e) => setEmail(e.target.value)}
            disabled={!!fixedEmail}
            required
          />
        </Field>
        <Field>
          <FieldLabel htmlFor="password">Password</FieldLabel>
          <Input
            id="password"
            type="password"
            autoComplete="new-password"
            minLength={10}
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
          />
          <FieldDescription>At least 10 characters.</FieldDescription>
        </Field>
        {error ? <FieldError>{errorMessage(error)}</FieldError> : null}
        <Button type="submit" disabled={pending}>
          Create account
        </Button>
      </FieldGroup>
    </form>
  );
}
