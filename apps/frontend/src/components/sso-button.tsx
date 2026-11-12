import { KeyRoundIcon } from "lucide-react";
import { Button } from "@/components/ui/button";

/**
 * Single sign-on: a plain link, as the browser must go to the provider.
 * `next` is where an app's sign-in goes on to; `invite` the invite page's
 * token, which makes the account.
 */
export function SsoButton({
  label,
  action = "Sign in with",
  next,
  invite,
  primary,
}: {
  /** The provider's label, "Single sign-on". */
  label: string;
  /** What comes before the label: "Sign in with", "Join with". */
  action?: string;
  next?: string;
  invite?: string;
  /** The main way in (no form beside it). */
  primary?: boolean;
}) {
  const params = new URLSearchParams();
  if (next) params.set("next", next);
  if (invite) params.set("invite", invite);
  const query = params.toString();
  return (
    <Button
      variant={primary ? "default" : "outline"}
      render={<a href={`/api/auth/oidc/login${query ? `?${query}` : ""}`} />}
    >
      <KeyRoundIcon />
      {action} {label}
    </Button>
  );
}
