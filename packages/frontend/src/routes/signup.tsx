import { createFileRoute, Link } from "@tanstack/react-router";
import { signup } from "organizer-sdk";
import { AuthCard } from "@/components/auth-card";
import { JoinOptions } from "@/components/join-options";

/** Open sign-up: the login page's "Create an account", when a superadmin allows it. */
export const Route = createFileRoute("/signup")({ component: Signup });

function Signup() {
  const { session } = Route.useRouteContext();
  return (
    <AuthCard
      title="Create an account"
      description={
        session.signup
          ? `Join Organizer on ${window.location.host}.`
          : "Sign-up is closed here: ask a superadmin for an invite."
      }
    >
      {session.signup && (
        <JoinOptions
          session={session}
          create={async (email, password) =>
            (await signup({ body: { email, password }, throwOnError: true })).data
          }
        />
      )}
      <p className="text-center text-sm text-muted-foreground">
        Have an account?{" "}
        <Link to="/login" className="underline underline-offset-4">
          Sign in
        </Link>
      </p>
    </AuthCard>
  );
}
