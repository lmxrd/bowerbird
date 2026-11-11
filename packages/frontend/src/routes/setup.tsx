import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { createFileRoute, useNavigate, useRouter } from "@tanstack/react-router";
import { setupMutation } from "organizer-sdk/react-query";
import { AuthCard } from "@/components/auth-card";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/api";

export const Route = createFileRoute("/setup")({ component: Setup });

function Setup() {
  const { session } = Route.useRouteContext();
  const router = useRouter();
  const navigate = useNavigate();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [filesRoot, setFilesRoot] = useState("");
  const setup = useMutation({
    ...setupMutation(),
    onSuccess: async () => {
      await router.invalidate();
      await navigate({ to: "/" });
    },
  });
  return (
    <AuthCard title="Set up Organizer" description="Create the first account. It's the superadmin.">
      <form
        onSubmit={(e) => {
          e.preventDefault();
          setup.mutate({
            body: { email, password, files_root: filesRoot.trim() || null },
          });
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
              value={email}
              onChange={(e) => setEmail(e.target.value)}
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
          {!session.files_locked && (
            <Field>
              <FieldLabel htmlFor="files-root">Folder of your files (optional)</FieldLabel>
              <Input
                id="files-root"
                placeholder="/mnt/nas/files"
                value={filesRoot}
                onChange={(e) => setFilesRoot(e.target.value)}
              />
              <FieldDescription>
                A folder on the server that may already hold files. Leave it empty for one in the
                data folder.
              </FieldDescription>
            </Field>
          )}
          {setup.isError && <FieldError>{errorMessage(setup.error)}</FieldError>}
          <Button type="submit" disabled={setup.isPending}>
            Create account
          </Button>
        </FieldGroup>
      </form>
    </AuthCard>
  );
}
