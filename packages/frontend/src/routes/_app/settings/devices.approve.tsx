import { useMutation } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { MonitorSmartphoneIcon } from "lucide-react";
import { deviceAuthorizeMutation } from "organizer-sdk/react-query";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { errorMessage } from "@/lib/api";

type Search = { redirect_uri?: string; code_challenge?: string; device_name?: string };

/** Where an app's sign-in in the browser lands: allow the device, or not. */
export const Route = createFileRoute("/_app/settings/devices/approve")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    redirect_uri: typeof search.redirect_uri === "string" ? search.redirect_uri : undefined,
    code_challenge: typeof search.code_challenge === "string" ? search.code_challenge : undefined,
    device_name: typeof search.device_name === "string" ? search.device_name : undefined,
  }),
  component: Approve,
});

function Approve() {
  const search = Route.useSearch();
  const navigate = useNavigate();
  const allow = useMutation({
    ...deviceAuthorizeMutation(),
    // To the app, with its one-time code.
    onSuccess: (approved) => window.location.assign(approved.redirect),
  });
  const complete = search.redirect_uri && search.code_challenge && search.device_name;
  return (
    <Card>
      <CardHeader>
        <MonitorSmartphoneIcon className="size-6 text-muted-foreground" />
        <CardTitle>
          {complete ? `Allow ${search.device_name} to access your files?` : "Nothing to approve"}
        </CardTitle>
        <CardDescription>
          {complete
            ? "It will be able to read and change your files. You can revoke it in Devices and passwords."
            : "Start the sign-in from the app again."}
        </CardDescription>
      </CardHeader>
      {complete && (
        <CardContent className="flex flex-col gap-4">
          <p className="text-xs text-muted-foreground">Returns to {search.redirect_uri}</p>
          {allow.isError && <p className="text-sm text-destructive">{errorMessage(allow.error)}</p>}
          <div className="flex gap-2">
            <Button
              disabled={allow.isPending}
              onClick={() =>
                allow.mutate({
                  body: {
                    redirect_uri: search.redirect_uri!,
                    code_challenge: search.code_challenge!,
                    device_name: search.device_name!,
                  },
                })
              }
            >
              Allow
            </Button>
            <Button variant="outline" onClick={() => navigate({ to: "/settings/devices" })}>
              Cancel
            </Button>
          </div>
        </CardContent>
      )}
    </Card>
  );
}
