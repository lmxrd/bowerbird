import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate, useRouter } from "@tanstack/react-router";
import type { TotpSetup } from "organizer-sdk";
import {
  loginMutation,
  loginTotpEnableMutation,
  loginTotpMutation,
  loginTotpSetupMutation,
} from "organizer-sdk/react-query";
import { AuthCard, OrSeparator } from "@/components/auth-card";
import { PasskeySignIn } from "@/components/passkey-sign-in";
import { RecoveryCodes, TotpSecret } from "@/components/recovery-codes";
import { SsoButton } from "@/components/sso-button";
import { Button } from "@/components/ui/button";
import { Field, FieldError, FieldGroup, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { allowedNext } from "@/lib/account";
import { errorMessage } from "@/lib/api";
import { passkeysSupported } from "@/lib/passkey";
import { ssoError } from "@/lib/sso";

type Search = { next?: string; error?: string; step?: "totp" | "totp-setup" };

export const Route = createFileRoute("/login")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    next: typeof search.next === "string" ? search.next : undefined,
    error: typeof search.error === "string" ? search.error : undefined,
    step: search.step === "totp" || search.step === "totp-setup" ? search.step : undefined,
  }),
  component: Login,
});

type Step = "credentials" | "code" | "enroll" | "codes";

function Login() {
  const search = Route.useSearch();
  const { session } = Route.useRouteContext();
  const router = useRouter();
  const navigate = useNavigate();
  const next = allowedNext(search.next);
  const [step, setStep] = useState<Step>(
    search.step === "totp" ? "code" : search.step === "totp-setup" ? "enroll" : "credentials",
  );
  const [codes, setCodes] = useState<string[]>([]);

  const done = async () => {
    await router.invalidate();
    if (next) await navigate({ href: next });
    else await navigate({ to: "/" });
  };

  return (
    <AuthCard
      title={step === "codes" ? "Your recovery codes" : "Sign in to Organizer"}
      description={
        step === "code"
          ? "Enter the code from your authenticator app, or a recovery code."
          : step === "enroll"
            ? "Two-factor authentication is required here. Set it up to go on."
            : step === "codes"
              ? "Two-factor authentication is on."
              : "Your files, filed for you."
      }
    >
      {ssoError(search.error) && step === "credentials" && (
        <p className="text-sm text-destructive">{ssoError(search.error)}</p>
      )}
      {step === "credentials" && (
        <>
          {/* Passkey first, then single sign-on, then the password. */}
          {session.passkeys && passkeysSupported() && <PasskeySignIn onDone={done} />}
          {session.oidc && <SsoButton label={session.oidc.label} next={next} />}
          {session.password_login && (session.oidc || session.passkeys) && <OrSeparator />}
          <Credentials
            passwords={session.password_login}
            primary={!(session.passkeys && passkeysSupported())}
            onStep={(totp) => (totp ? setStep("code") : setStep("enroll"))}
            onDone={done}
          />
          {session.signup && !next && (
            <p className="text-center text-sm text-muted-foreground">
              New here?{" "}
              <Link to="/signup" className="underline underline-offset-4">
                Create an account
              </Link>
            </p>
          )}
        </>
      )}
      {step === "code" && <CodeStep onDone={done} />}
      {step === "enroll" && (
        <Enroll
          onEnabled={(c) => {
            setCodes(c);
            setStep("codes");
          }}
        />
      )}
      {step === "codes" && (
        <>
          <RecoveryCodes codes={codes} />
          <Button onClick={done}>Continue</Button>
        </>
      )}
    </AuthCard>
  );
}

function Credentials({
  passwords,
  primary,
  onStep,
  onDone,
}: {
  passwords: boolean;
  /** The main way in (no passkey button above it). */
  primary: boolean;
  onStep: (totp: boolean) => void;
  onDone: () => void;
}) {
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const login = useMutation({
    ...loginMutation(),
    onSuccess: (step) => {
      if (step?.totp_required) onStep(true);
      else if (step?.totp_setup_required) onStep(false);
      else onDone();
    },
  });
  if (!passwords) return null;
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        login.mutate({ body: { email, password } });
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
            autoComplete="current-password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            required
          />
          {login.isError && <FieldError>{errorMessage(login.error)}</FieldError>}
        </Field>
        <Button type="submit" variant={primary ? "default" : "outline"} disabled={login.isPending}>
          Sign in
        </Button>
      </FieldGroup>
    </form>
  );
}

function CodeStep({ onDone }: { onDone: () => void }) {
  const [code, setCode] = useState("");
  const verify = useMutation({ ...loginTotpMutation(), onSuccess: onDone });
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault();
        verify.mutate({ body: { code } });
      }}
    >
      <FieldGroup>
        <Field>
          <FieldLabel htmlFor="code">Code</FieldLabel>
          <Input
            id="code"
            autoComplete="one-time-code"
            inputMode="text"
            placeholder="123456"
            value={code}
            onChange={(e) => setCode(e.target.value)}
            required
          />
          {verify.isError && <FieldError>{errorMessage(verify.error)}</FieldError>}
        </Field>
        <Button type="submit" disabled={verify.isPending}>
          Verify
        </Button>
      </FieldGroup>
    </form>
  );
}

function Enroll({ onEnabled }: { onEnabled: (codes: string[]) => void }) {
  const [secret, setSecret] = useState<TotpSetup>();
  const [code, setCode] = useState("");
  const start = useMutation({ ...loginTotpSetupMutation(), onSuccess: setSecret });
  const enable = useMutation({
    ...loginTotpEnableMutation(),
    onSuccess: (r) => onEnabled(r.codes),
  });
  if (!secret) {
    return (
      <Button onClick={() => start.mutate({})} disabled={start.isPending}>
        Set up two-factor authentication
      </Button>
    );
  }
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        enable.mutate({ body: { code } });
      }}
    >
      <TotpSecret secret={secret.secret} uri={secret.uri} />
      <Field>
        <FieldLabel htmlFor="code">Code from the app</FieldLabel>
        <Input
          id="code"
          autoComplete="one-time-code"
          inputMode="numeric"
          placeholder="123456"
          value={code}
          onChange={(e) => setCode(e.target.value)}
          required
        />
        {enable.isError && <FieldError>{errorMessage(enable.error)}</FieldError>}
      </Field>
      <Button type="submit" disabled={enable.isPending}>
        Turn on
      </Button>
    </form>
  );
}
