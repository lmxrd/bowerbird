import { Link, useRouterState } from "@tanstack/react-router";

import {
  SidebarGroup,
  SidebarGroupLabel,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
} from "@/components/ui/sidebar";

export type NavItem = {
  title: string;
  url: string;
  /** The link's query: the trash of the drive the user is in. */
  search?: Record<string, string>;
  icon?: React.ReactNode;
  /** A second line under the title: a drive's server on another server. */
  detail?: string;
  /** A count beside the entry, shown when above zero. */
  badge?: number;
  /** Whether the page shown is this entry's; by default, its URL or one under it. */
  active?: (path: string) => boolean;
};

export function NavMain({ label, items }: { label?: string; items: NavItem[] }) {
  const path = useRouterState({ select: (s) => s.location.pathname });
  const active = (item: NavItem) =>
    item.active ? item.active(path) : path === item.url || path.startsWith(`${item.url}/`);
  return (
    <SidebarGroup>
      {label && <SidebarGroupLabel>{label}</SidebarGroupLabel>}
      <SidebarMenu>
        {items.map((item) => (
          <SidebarMenuItem key={item.url}>
            <SidebarMenuButton
              tooltip={item.title}
              isActive={active(item)}
              size={item.detail ? "lg" : "default"}
              render={<Link to={item.url} search={item.search} />}
            >
              {item.icon}
              {item.detail ? (
                <span className="grid flex-1 text-left leading-tight">
                  <span className="truncate">{item.title}</span>
                  <span className="truncate text-xs text-muted-foreground">{item.detail}</span>
                </span>
              ) : (
                <span>{item.title}</span>
              )}
            </SidebarMenuButton>
            {item.badge ? <SidebarMenuBadge>{item.badge}</SidebarMenuBadge> : null}
          </SidebarMenuItem>
        ))}
      </SidebarMenu>
    </SidebarGroup>
  );
}
