import { UsersIcon } from "lucide-react";
import { Avatar, AvatarFallback } from "@/components/ui/avatar";
import type { Recipient } from "@/lib/sharing";
import { initials } from "@/lib/sharing";

/** Someone with access (here or on another server): their initials, or a team's icon. */
export function RecipientIcon({ kind, label }: { kind: Recipient["kind"]; label: string }) {
  return (
    <Avatar size="sm">
      <AvatarFallback>
        {kind === "team" ? <UsersIcon className="size-3.5" /> : initials(label)}
      </AvatarFallback>
    </Avatar>
  );
}
