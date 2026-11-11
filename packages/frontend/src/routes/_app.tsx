import { useEffect } from "react";
import { createFileRoute, Outlet, redirect } from "@tanstack/react-router";
import { AppSidebar } from "@/components/app-sidebar";
import { StepUpProvider } from "@/components/step-up";
import { SidebarInset, SidebarProvider } from "@/components/ui/sidebar";
import { queryClient } from "@/lib/api";

/** The signed-in app: the sidebar, and the page beside it. */
export const Route = createFileRoute("/_app")({
  beforeLoad: ({ context }) => {
    const user = context.session.user;
    if (!user) throw redirect({ to: "/login" });
    return { user };
  },
  component: App,
});

function App() {
  const { user } = Route.useRouteContext();
  // The server says when the files changed (here, in another tab, an app,
  // WebDAV or on disk), and when the drop box's items did (the AI's
  // analysis, an accepted item): what is shown is fetched again.
  useEffect(() => {
    const events = new EventSource("/api/events");
    events.addEventListener("changes", () => {
      void queryClient.invalidateQueries();
    });
    events.addEventListener("drop-box", () => {
      for (const _id of ["listDropBoxItems", "getDropBoxItem"]) {
        void queryClient.invalidateQueries({ queryKey: [{ _id }] });
      }
    });
    return () => events.close();
  }, []);
  return (
    <StepUpProvider>
      <SidebarProvider>
        <AppSidebar user={user} />
        <SidebarInset>
          <Outlet />
        </SidebarInset>
      </SidebarProvider>
    </StepUpProvider>
  );
}
