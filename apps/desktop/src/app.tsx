import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { InboxIcon, RefreshCwIcon, SettingsIcon } from "lucide-react";
import {
  Sidebar,
  SidebarContent,
  SidebarGroup,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
} from "@/components/ui/sidebar";
import { dropBoxItemsOptions, queryClient } from "@/lib/api";
import { waitingCount } from "@/lib/drop-box";
import { DropBoxPage } from "./pages/drop-box";
import { SettingsPage } from "./pages/settings";
import { SignIn } from "./pages/sign-in";
import { SyncPage } from "./pages/sync";
import { type Page, type View, useAppEvent, useView } from "./lib/desktop";
import { host } from "./lib/status";

/** The window: the sign-in, or the pages with their navigation. */
export function App() {
  const view = useView();
  const [page, setPage] = useState<Page>("sync");
  useAppEvent<Page>("navigate", setPage);
  // The drop box changed on the server: its items again.
  useAppEvent("drop-box", () => void queryClient.invalidateQueries());

  if (!view) return null;
  if (!view.signed_in) return <SignIn />;
  return <Shell view={view} page={page} onPage={setPage} />;
}

const pages: { page: Page; title: string; icon: React.ReactNode }[] = [
  { page: "sync", title: "Sync", icon: <RefreshCwIcon /> },
  { page: "drop-box", title: "Drop box", icon: <InboxIcon /> },
  { page: "settings", title: "Settings", icon: <SettingsIcon /> },
];

function Shell({ view, page, onPage }: { view: View; page: Page; onPage: (page: Page) => void }) {
  // The same count as the web's sidebar: the items that wait for the user.
  const items = useQuery(dropBoxItemsOptions());
  const waiting = items.data ? waitingCount(items.data) : view.waiting;
  return (
    <SidebarProvider>
      <Sidebar collapsible="none" className="h-svh border-r">
        <SidebarHeader>
          <div className="flex flex-col px-2 py-1.5">
            <span className="text-sm font-medium">Organizer</span>
            <span className="truncate text-xs text-muted-foreground">{host(view.server)}</span>
          </div>
        </SidebarHeader>
        <SidebarContent>
          <SidebarGroup>
            <SidebarMenu>
              {pages.map((p) => (
                <SidebarMenuItem key={p.page}>
                  <SidebarMenuButton isActive={page === p.page} onClick={() => onPage(p.page)}>
                    {p.icon}
                    <span>{p.title}</span>
                  </SidebarMenuButton>
                  {p.page === "drop-box" && waiting > 0 ? (
                    <SidebarMenuBadge>{waiting}</SidebarMenuBadge>
                  ) : null}
                </SidebarMenuItem>
              ))}
            </SidebarMenu>
          </SidebarGroup>
        </SidebarContent>
      </Sidebar>
      <SidebarInset className="h-svh overflow-y-auto">
        {page === "sync" && <SyncPage view={view} onSettings={() => onPage("settings")} />}
        {page === "drop-box" && <DropBoxPage />}
        {page === "settings" && <SettingsPage view={view} />}
      </SidebarInset>
    </SidebarProvider>
  );
}
