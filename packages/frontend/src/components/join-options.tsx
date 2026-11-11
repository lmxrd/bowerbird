import { useMutation } from "@tanstack/react-query";
import { useNavigate, useRouter } from "@tanstack/react-router";
import type { Session } from "organizer-sdk";
import { OrSeparator } from "@/components/auth-card";
import { NewAccountForm } from "@/components/new-account-form";
import { SsoButton } from "@/components/sso-button";
import { afterNewAccount } from "@/lib/account";

/**
 * The ways to make an account: an email and a password, or single sign-on
 * (only that with `OIDC_ONLY`). The invite page and open sign-up.
 */
export function JoinOptions({
  session,
  email,
  invite,
  create,
}: {
  session: Session;
  /** The invite's email, filled in and locked. */
  email?: string | null;
  /** The invite's token, for single sign-on. */
  invite?: string;
  /** Makes the account: the server's answer (nothing, or a two-factor step). */
  create: (email: string, password: string) => Promise<unknown>;
}) {
  const router = useRouter();
  const navigate = useNavigate();
  const made = useMutation({
    mutationFn: ({ email, password }: { email: string; password: string }) =>
      create(email, password),
    onSuccess: async (answer) => {
      const next = afterNewAccount(answer);
      if (next === "app") {
        await router.invalidate();
        await navigate({ to: "/" });
      } else {
        await navigate({ to: "/login", search: { step: next } });
      }
    },
  });
  // With an invite, single sign-on makes the account; without one, only
  // when it is the only way in (open sign-up is single sign-on's then).
  const sso = session.oidc && (invite || !session.password_login) ? session.oidc : null;
  return (
    <>
      {session.password_login && (
        <NewAccountForm
          email={email}
          pending={made.isPending}
          error={made.error}
          onSubmit={(email, password) => made.mutate({ email, password })}
        />
      )}
      {session.password_login && sso && <OrSeparator />}
      {sso && (
        <SsoButton
          label={sso.label}
          action="Continue with"
          invite={invite}
          primary={!session.password_login}
        />
      )}
    </>
  );
}
