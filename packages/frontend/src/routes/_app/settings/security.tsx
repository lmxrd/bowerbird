import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { createFileRoute, useRouter } from "@tanstack/react-router";
import { LockIcon } from "lucide-react";
import { toast } from "sonner";
import type { TotpSetup } from "organizer-sdk";
import {
  confirmReplaceTotpMutation,
  disableTotpMutation,
  enableTotpMutation,
  regenerateRecoveryCodesMutation,
  replaceTotpMutation,
  setupTotpMutation,
} from "organizer-sdk/react-query";
import { AccountActivity } from "@/components/account-activity";
import { LevelSettingsCard, useMySettings } from "@/components/levels";
import { PasskeysCard } from "@/components/passkeys-card";
import { SuperadminGate } from "@/components/step-up";
import { RecoveryCodes, TotpSecret } from "@/components/recovery-codes";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardAction,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Field, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/api";
import { jsonOf, sourceName } from "@/lib/levels";

export const Route = createFileRoute("/_app/settings/security")({ component: Security });

/** What is being done: nothing, setting up, or an action that needs a code. */
type Mode =
  | { kind: "idle" }
  | { kind: "setup"; secret: TotpSetup }
  | { kind: "codes"; codes: string[] }
  | { kind: "confirm"; action: "disable" | "recovery" | "replace" }
  | { kind: "replace"; secret: TotpSetup };

function Security() {
  const { user, session } = Route.useRouteContext();
  const router = useRouter();
  const [mode, setMode] = useState<Mode>({ kind: "idle" });
  const [code, setCode] = useState("");
  // Two-factor required by the superadmin or a team: who requires it.
  const required = useMySettings().get("sign_in.two_factor_required");
  const requiredBy =
    required && jsonOf(required.value) === true
      ? sourceName(required.locked_by ?? required.source)
      : undefined;
  const reset = async () => {
    setCode("");
    await router.invalidate();
  };
  const fail = (error: unknown) => toast.error(errorMessage(error));
  const setup = useMutation({
    ...setupTotpMutation(),
    onSuccess: (secret) => setMode({ kind: "setup", secret }),
    onError: fail,
  });
  const enable = useMutation({
    ...enableTotpMutation(),
    onSuccess: async (r) => {
      setMode({ kind: "codes", codes: r.codes });
      await reset();
    },
  });
  const disable = useMutation({
    ...disableTotpMutation(),
    onSuccess: async () => {
      setMode({ kind: "idle" });
      toast.success("Two-factor authentication is off");
      await reset();
    },
  });
  const recovery = useMutation({
    ...regenerateRecoveryCodesMutation(),
    onSuccess: (r) => {
      setCode("");
      setMode({ kind: "codes", codes: r.codes });
    },
  });
  const replace = useMutation({
    ...replaceTotpMutation(),
    onSuccess: (secret) => {
      setCode("");
      setMode({ kind: "replace", secret });
    },
  });
  const confirmReplace = useMutation({
    ...confirmReplaceTotpMutation(),
    onSuccess: async () => {
      setMode({ kind: "idle" });
      toast.success("Your new authenticator app is in use");
      await reset();
    },
  });
  const pending = [enable, disable, recovery, replace, confirmReplace].find((m) => m.isError);

  const codeForm = (label: string, submit: () => void, button: string) => (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <Field>
        <FieldLabel htmlFor="code">{label}</FieldLabel>
        <Input
          id="code"
          autoComplete="one-time-code"
          placeholder="123456"
          value={code}
          onChange={(e) => setCode(e.target.value)}
          required
        />
        {pending && <FieldError>{errorMessage(pending.error)}</FieldError>}
      </Field>
      <div className="flex gap-2">
        <Button type="submit">{button}</Button>
        <Button type="button" variant="outline" onClick={() => setMode({ kind: "idle" })}>
          Cancel
        </Button>
      </div>
    </form>
  );

  return (
    <>
      <PasskeysCard available={session.passkeys} />
      <Card>
        <CardHeader>
          <CardTitle>Two-factor authentication</CardTitle>
          <CardDescription>
            A code from your authenticator app is asked for at sign-in.
          </CardDescription>
          <CardAction>
            <Badge variant={user.totp_enabled ? "default" : "outline"}>
              {user.totp_enabled ? "On" : "Off"}
            </Badge>
          </CardAction>
        </CardHeader>
        <CardContent className="flex flex-col gap-4">
          {mode.kind === "idle" && !user.totp_enabled && (
            <Button className="self-start" onClick={() => setup.mutate({})}>
              Set up
            </Button>
          )}
          {mode.kind === "idle" && user.totp_enabled && (
            <div className="flex flex-wrap gap-2">
              <Button
                variant="outline"
                onClick={() => setMode({ kind: "confirm", action: "recovery" })}
              >
                New recovery codes
              </Button>
              <Button
                variant="outline"
                onClick={() => setMode({ kind: "confirm", action: "replace" })}
              >
                Replace app
              </Button>
              {!session.totp_required && (
                <Button
                  variant="destructive"
                  onClick={() => setMode({ kind: "confirm", action: "disable" })}
                >
                  Turn off
                </Button>
              )}
            </div>
          )}
          {mode.kind === "setup" && (
            <>
              <TotpSecret secret={mode.secret.secret} uri={mode.secret.uri} />
              {codeForm("Code from the app", () => enable.mutate({ body: { code } }), "Turn on")}
            </>
          )}
          {mode.kind === "codes" && (
            <>
              <RecoveryCodes codes={mode.codes} />
              <Button
                variant="outline"
                className="self-start"
                onClick={() => setMode({ kind: "idle" })}
              >
                Done
              </Button>
            </>
          )}
          {mode.kind === "confirm" &&
            codeForm(
              "Code from your current app, or a recovery code",
              () => {
                const body = { body: { code } };
                if (mode.action === "disable") disable.mutate(body);
                if (mode.action === "recovery") recovery.mutate(body);
                if (mode.action === "replace") replace.mutate(body);
              },
              mode.action === "disable" ? "Turn off" : "Continue",
            )}
          {mode.kind === "replace" && (
            <>
              <TotpSecret secret={mode.secret.secret} uri={mode.secret.uri} />
              {codeForm(
                "Code from the new app",
                () => confirmReplace.mutate({ body: { code } }),
                "Use the new app",
              )}
            </>
          )}
          {requiredBy && (
            <p className="text-sm text-muted-foreground">
              <LockIcon className="inline size-3" /> Required: set by {requiredBy}
            </p>
          )}
        </CardContent>
      </Card>
      <AccountActivity />
      {user.is_superadmin && (
        <SuperadminGate lazy title="For everyone">
          <LevelSettingsCard
            scope={{ level: "superadmin" }}
            prefixes={["sign_in."]}
            title="For everyone"
            description="Whether every user must sign in with two-factor authentication; a team may require it for its members."
          />
        </SuperadminGate>
      )}
    </>
  );
}
