import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { CheckIcon, GlobeIcon, TriangleAlertIcon, XIcon } from "lucide-react";
import { toast } from "sonner";
import type { Drive } from "organizer-sdk";
import { acceptSharedMutation, declineSharedMutation } from "organizer-sdk/react-query";
import { Alert, AlertAction, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { errorMessage, queryClient } from "@/lib/api";
import { remoteFrom } from "@/lib/federation";

/**
 * A drive shared from another server, at its top (docs/M7E_DESIGN.md §6):
 * Accept / Decline while it waits for the user, and why its server could
 * not be reached when the last look failed.
 */
export function RemoteDriveBanner({ drive }: { drive: Drive }) {
  const navigate = useNavigate();
  const failed = (error: unknown) => toast.error(errorMessage(error));
  const refresh = () => void queryClient.invalidateQueries();
  const accept = useMutation({
    ...acceptSharedMutation(),
    onSuccess: () => {
      refresh();
      toast.success(`Accepted ${drive.name}`);
    },
    onError: failed,
  });
  const decline = useMutation({
    ...declineSharedMutation(),
    onSuccess: () => {
      refresh();
      toast.success(`Declined ${drive.name}`);
      void navigate({ to: "/" });
    },
    onError: failed,
  });
  const remote = drive.remote;
  if (!remote) return null;
  const path = { id: drive.root_id ?? "" };
  return (
    <>
      {remote.state === "pending" && drive.root_id && (
        <Alert>
          <GlobeIcon />
          <AlertDescription>
            {remoteFrom(remote)} shared this drive with you. Accept it to keep it, or decline it.
          </AlertDescription>
          <AlertAction className="flex gap-2">
            <Button size="sm" disabled={accept.isPending} onClick={() => accept.mutate({ path })}>
              <CheckIcon />
              Accept
            </Button>
            <Button
              size="sm"
              variant="outline"
              disabled={decline.isPending}
              onClick={() => decline.mutate({ path })}
            >
              <XIcon />
              Decline
            </Button>
          </AlertAction>
        </Alert>
      )}
      {remote.error && (
        <Alert variant="destructive">
          <TriangleAlertIcon />
          <AlertDescription>
            {remote.server} isn't reachable now: {remote.error}. What you see is what it held last.
          </AlertDescription>
        </Alert>
      )}
    </>
  );
}
