import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, useRouter } from "@tanstack/react-router";
import { toast } from "sonner";
import {
  changeEmailMutation,
  changePasswordMutation,
  getOwnStorageOptions,
} from "organizer-sdk/react-query";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Progress } from "@/components/ui/progress";
import { errorMessage } from "@/lib/api";
import { formatUsage, usagePercent } from "@/lib/users";

export const Route = createFileRoute("/_app/settings/")({ component: Account });

function Account() {
  const { user, session } = Route.useRouteContext();
  const router = useRouter();
  const [email, setEmail] = useState(user.email);
  const [current, setCurrent] = useState("");
  const [password, setPassword] = useState("");
  const changeEmail = useMutation({
    ...changeEmailMutation(),
    onSuccess: async () => {
      toast.success("Email changed");
      await router.invalidate();
    },
  });
  const changePassword = useMutation({
    ...changePasswordMutation(),
    onSuccess: async () => {
      toast.success("Password changed. Your other sessions are signed out.");
      setCurrent("");
      setPassword("");
      await router.invalidate();
    },
  });
  return (
    <>
      <OwnStorage />
      <Card>
        <CardHeader>
          <CardTitle>Email</CardTitle>
          <CardDescription>What you sign in with.</CardDescription>
        </CardHeader>
        <CardContent>
          <form
            onSubmit={(e) => {
              e.preventDefault();
              changeEmail.mutate({ body: { email } });
            }}
          >
            <FieldGroup>
              <Field>
                <FieldLabel htmlFor="email">Email</FieldLabel>
                <Input
                  id="email"
                  type="email"
                  value={email}
                  onChange={(e) => setEmail(e.target.value)}
                  required
                />
                {changeEmail.isError && <FieldError>{errorMessage(changeEmail.error)}</FieldError>}
              </Field>
              <Button
                type="submit"
                variant="outline"
                className="self-start"
                disabled={changeEmail.isPending}
              >
                Save email
              </Button>
            </FieldGroup>
          </form>
        </CardContent>
      </Card>
      {session.password_login && (
        <Card>
          <CardHeader>
            <CardTitle>Password</CardTitle>
            <CardDescription>
              {user.has_password
                ? "Changing it signs your other sessions out."
                : "You sign in with single sign-on. Set a password to sign in without it too."}
            </CardDescription>
          </CardHeader>
          <CardContent>
            <form
              onSubmit={(e) => {
                e.preventDefault();
                changePassword.mutate({
                  body: {
                    current_password: user.has_password ? current : null,
                    new_password: password,
                  },
                });
              }}
            >
              <FieldGroup>
                {user.has_password && (
                  <Field>
                    <FieldLabel htmlFor="current">Current password</FieldLabel>
                    <Input
                      id="current"
                      type="password"
                      autoComplete="current-password"
                      value={current}
                      onChange={(e) => setCurrent(e.target.value)}
                      required
                    />
                  </Field>
                )}
                <Field>
                  <FieldLabel htmlFor="new">New password</FieldLabel>
                  <Input
                    id="new"
                    type="password"
                    autoComplete="new-password"
                    minLength={10}
                    value={password}
                    onChange={(e) => setPassword(e.target.value)}
                    required
                  />
                  {changePassword.isError && (
                    <FieldError>{errorMessage(changePassword.error)}</FieldError>
                  )}
                </Field>
                <Button
                  type="submit"
                  variant="outline"
                  className="self-start"
                  disabled={changePassword.isPending}
                >
                  Change password
                </Button>
              </FieldGroup>
            </form>
          </CardContent>
        </Card>
      )}
    </>
  );
}

/** What your files, trash, versions and drop box take, of your limit. */
function OwnStorage() {
  const storage = useQuery(getOwnStorageOptions());
  const data = storage.data;
  const percent = data ? usagePercent(data.used, data.limit) : undefined;
  return (
    <SettingsCard
      title="Storage"
      description="Your files, trash, versions and what waits in the drop box."
      error={storage.error}
    >
      {data && (
        <div className="flex flex-col gap-2">
          <p className="text-sm">
            {formatUsage(data.used, data.limit)}
            {data.limit == null && <span className="text-muted-foreground"> · no limit</span>}
          </p>
          {percent !== undefined && <Progress value={percent} aria-label="Storage used" />}
        </div>
      )}
    </SettingsCard>
  );
}
