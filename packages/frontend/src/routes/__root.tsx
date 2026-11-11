import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Outlet, redirect } from "@tanstack/react-router";
import { getSessionOptions } from "organizer-sdk/react-query";
import { allowedNext } from "@/lib/account";

export const Route = createRootRouteWithContext<{ queryClient: QueryClient }>()({
  // Every page goes through here: first run → /setup, signed out → /login.
  beforeLoad: async ({ location, context }) => {
    // Always fresh: the session changes with sign-ins and settings.
    const session = await context.queryClient.fetchQuery({
      ...getSessionOptions(),
      staleTime: 0,
    });
    const path = location.pathname;
    // The pages for someone who is not signed in: the login, open sign-up
    // and an invite's.
    const signedOutPage = path === "/login" || path === "/signup" || path.startsWith("/invite/");
    // A public link's page: anyone, signed in or not.
    if (path.startsWith("/s/")) return { session };
    if (session.needsSetup && path !== "/setup") throw redirect({ to: "/setup" });
    if (!session.needsSetup && !session.user && !signedOutPage) {
      throw redirect({ to: "/login", search: { next: allowedNext(location.href) } });
    }
    if (session.user && (signedOutPage || path === "/setup")) {
      // Signed in on the login page: on to the page the sign-in was for
      // (a reload of the page in between included), else the top.
      const next =
        path === "/login" ? allowedNext((location.search as { next?: unknown }).next) : undefined;
      if (next) throw redirect({ href: next });
      throw redirect({ to: "/" });
    }
    return { session };
  },
  component: Outlet,
});
