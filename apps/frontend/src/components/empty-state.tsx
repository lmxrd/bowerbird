import type { LucideIcon } from "lucide-react";
import { cn } from "@/lib/utils";

/** What a page or list shows when there is nothing in it yet. */
export function EmptyState({
  icon: Icon,
  title,
  children,
  action,
  className,
}: {
  icon: LucideIcon;
  title: string;
  /** One line on what goes here. */
  children?: React.ReactNode;
  /** A button under it: "Unlock". */
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex flex-1 flex-col items-center justify-center gap-2 rounded-lg border border-dashed p-10 text-center",
        className,
      )}
    >
      <Icon className="size-6 text-muted-foreground" />
      <p className="font-medium">{title}</p>
      {children && <p className="text-sm text-muted-foreground">{children}</p>}
      {action && <div className="mt-2">{action}</div>}
    </div>
  );
}
