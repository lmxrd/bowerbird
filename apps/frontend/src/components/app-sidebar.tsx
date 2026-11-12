import * as React from "react";
import { Link, useRouterState } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import {
  BoxIcon,
  FolderSyncIcon,
  GlobeIcon,
  HardDriveIcon,
  InboxIcon,
  SettingsIcon,
  Share2Icon,
  Trash2Icon,
  UsersIcon,
} from "lucide-react";
import type { User } from "organizer-sdk";

import { type NavItem, NavMain } from "@/components/nav-main";
import { NavUser } from "@/components/nav-user";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarHeader,
  SidebarMenu,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarRail,
} from "@/components/ui/sidebar";
import { dropBoxItemsOptions, sharedWithMeOptions } from "@/lib/api";
import { drivesOptions } from "@/lib/drive-files";
import { waitingCount } from "@/lib/drop-box";
import { MY_DRIVE, driveUrl } from "@/lib/drives";
import { remoteDriveLine } from "@/lib/federation";
import { newCount } from "@/lib/sharing";

export function AppSidebar({
  user,
  ...props
}: React.ComponentProps<typeof Sidebar> & { user: User }) {
  const items = useQuery(dropBoxItemsOptions());
  const shared = useQuery(sharedWithMeOptions());
  // The drives come again with every live `changes` event (`_app.tsx`
  // fetches everything again): a drive joined, left or renamed shows.
  const drives = useQuery(drivesOptions());
  const sharedDrives = drives.data?.filter((d) => d.kind === "shared") ?? [];
  // Mirrors are read-only drives of the user's (docs/M7D_DESIGN.md §5).
  const mirrors = drives.data?.filter((d) => d.kind === "mirror") ?? [];
  const path = useRouterState({ select: (s) => s.location.pathname });
  // The trash opens on the drive the user is in.
  const inDrive = [...sharedDrives, ...mirrors].find((d) => {
    const url = driveUrl(d, "/");
    return path === url || path.startsWith(`${url}/`);
  });
  const navDrives: NavItem[] = [
    {
      title: MY_DRIVE.name,
      url: "/",
      icon: <HardDriveIcon />,
      active: (p) => p === "/" || p.startsWith("/files/"),
    },
    ...sharedDrives.map((drive) => ({
      title: drive.name,
      url: driveUrl(drive, "/"),
      icon: <UsersIcon />,
    })),
    {
      title: "Shared with me",
      url: "/shared",
      icon: <Share2Icon />,
      badge: newCount(shared.data),
    },
  ];
  const navMirrors: NavItem[] = mirrors.map((drive) => ({
    title: drive.name,
    url: driveUrl(drive, "/"),
    icon: <FolderSyncIcon />,
  }));
  // Whole drives shared from other servers (docs/M7E_DESIGN.md §6), with
  // their server and whether they wait to be accepted or fail.
  const navRemote: NavItem[] = (drives.data ?? [])
    .filter((d) => d.kind === "remote")
    .map((drive) => ({
      title: drive.name,
      url: driveUrl(drive, "/"),
      icon: <GlobeIcon />,
      detail: drive.remote ? remoteDriveLine(drive.remote, drive.available) : undefined,
    }));
  const navMain: NavItem[] = [
    {
      title: "Drop box",
      url: "/drop-box",
      icon: <InboxIcon />,
      badge: items.data ? waitingCount(items.data) : 0,
    },
    {
      title: "Trash",
      url: "/trash",
      search: inDrive ? { drive: inDrive.id } : undefined,
      icon: <Trash2Icon />,
    },
    { title: "Settings", url: "/settings", icon: <SettingsIcon /> },
  ];
  return (
    <Sidebar collapsible="icon" {...props}>
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton size="lg" render={<Link to="/" />}>
              <div className="flex aspect-square size-8 items-center justify-center rounded-lg bg-sidebar-primary text-sidebar-primary-foreground">
                <BoxIcon className="size-4" />
              </div>
              <span className="truncate font-medium">Organizer</span>
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarHeader>
      <SidebarContent>
        <NavMain label="Drives" items={navDrives} />
        {navRemote.length > 0 && <NavMain label="Other servers" items={navRemote} />}
        {navMirrors.length > 0 && <NavMain label="Mirrors" items={navMirrors} />}
        <NavMain items={navMain} />
      </SidebarContent>
      <SidebarFooter>
        <NavUser user={user} />
      </SidebarFooter>
      <SidebarRail />
    </Sidebar>
  );
}
