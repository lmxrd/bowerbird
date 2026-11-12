import { createFileRoute, Link, Outlet, useRouterState } from "@tanstack/react-router";
import { useQuery } from "@tanstack/react-query";
import { listBackendsOptions, listMirrorsOptions } from "organizer-sdk/react-query";
import { useMySettings } from "@/components/levels";
import { PageHeader } from "@/components/page-header";
import { Button } from "@/components/ui/button";

export const Route = createFileRoute("/_app/settings")({ component: Settings });

const sections = [
  { to: "/settings", label: "Account" },
  { to: "/settings/security", label: "Security" },
  { to: "/settings/teams", label: "Teams" },
  { to: "/settings/contacts", label: "Contacts" },
  { to: "/settings/devices", label: "Devices and passwords" },
  { to: "/settings/encryption", label: "Encryption" },
  { to: "/settings/notifications", label: "Notifications" },
  { to: "/settings/drop-box", label: "Drop box" },
  { to: "/settings/retention", label: "Trash and versions" },
] as const;
const yourStorage = { to: "/settings/storage/yours", label: "Your storage" } as const;
const mirrorsSection = { to: "/settings/mirrors", label: "Mirrors" } as const;
const superadminSections = [
  { to: "/settings/users", label: "Users" },
  { to: "/settings/drives", label: "Drives" },
  { to: "/settings/storage", label: "Storage" },
  { to: "/settings/sso", label: "Single sign-on" },
  { to: "/settings/federation", label: "Federation" },
  { to: "/settings/ai", label: "AI" },
  { to: "/settings/backups", label: "Backups" },
] as const;

function Settings() {
  const { user } = Route.useRouteContext();
  const path = useRouterState({ select: (s) => s.location.pathname });
  // Your storage: when allowed, or while the user still has some.
  const mine = useMySettings();
  const own = useQuery(listBackendsOptions());
  const ownStorage =
    mine.get("storage.own_backends")?.value === true || (own.data?.length ?? 0) > 0;
  // Mirrors: with storage of one's own to put them in, or while one has some.
  const mirrors = useQuery(listMirrorsOptions());
  const hasMirrors = ownStorage || (mirrors.data?.length ?? 0) > 0;
  const all = [
    ...sections,
    ...(ownStorage ? [yourStorage] : []),
    ...(hasMirrors ? [mirrorsSection] : []),
    ...(user.is_superadmin ? superadminSections : []),
  ];
  const current = all.find((s) =>
    s.to === "/settings" ? path === "/settings" || path === "/settings/" : path.startsWith(s.to),
  );
  return (
    <>
      <PageHeader
        crumbs={[
          { label: "Settings", to: "/settings" },
          ...(current ? [{ label: current.label }] : []),
        ]}
      />
      <div className="flex flex-1 flex-col gap-6 p-4 md:flex-row">
        <nav className="flex shrink-0 gap-1 overflow-x-auto md:w-48 md:flex-col">
          {all.map((section) => (
            <Button
              key={section.to}
              variant={section === current ? "secondary" : "ghost"}
              className="justify-start"
              render={<Link to={section.to} />}
            >
              {section.label}
            </Button>
          ))}
        </nav>
        <div className="flex min-w-0 max-w-2xl flex-1 flex-col gap-6">
          <Outlet />
        </div>
      </div>
    </>
  );
}
