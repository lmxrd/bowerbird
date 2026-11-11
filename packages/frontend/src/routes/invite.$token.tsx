import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { acceptInvite } from "organizer-sdk";
import { getInviteOptions } from "organizer-sdk/react-query";
import { AuthCard } from "@/components/auth-card";
import { JoinOptions } from "@/components/join-options";
import { Skeleton } from "@/components/ui/skeleton";
import { errorMessage } from "@/lib/api";

/** An invite link: make the account it invites, with a password or single sign-on. */
export const Route = createFileRoute("/invite/$token")({ component: Invite });

function Invite() {
  const { session } = Route.useRouteContext();
  const { token } = Route.useParams();
  const invite = useQuery(getInviteOptions({ path: { token } }));
  return (
    <AuthCard title="Join Organizer" description={`${window.location.host} invited you.`}>
      {invite.isPending && <Skeleton className="h-40" />}
      {invite.isError && (
        <>
          <p className="text-sm text-destructive">{errorMessage(invite.error)}</p>
          <p className="text-center text-sm text-muted-foreground">
            Have an account?{" "}
            <Link to="/login" className="underline underline-offset-4">
              Sign in
            </Link>
          </p>
        </>
      )}
      {invite.data && (
        <JoinOptions
          session={session}
          email={invite.data.email}
          invite={token}
          create={async (email, password) =>
            (
              await acceptInvite({
                path: { token },
                body: { email, password },
                throwOnError: true,
              })
            ).data
          }
        />
      )}
    </AuthCard>
  );
}
