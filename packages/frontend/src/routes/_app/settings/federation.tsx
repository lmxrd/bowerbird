import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { PencilIcon } from "lucide-react";
import { toast } from "sonner";
import type { FederationMode, FederationSettings } from "organizer-sdk";
import { getFederationOptions, setFederationMutation } from "organizer-sdk/react-query";
import { FormDialog } from "@/components/form-dialog";
import { LevelSettingsCard } from "@/components/levels";
import { SettingsCard } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Textarea } from "@/components/ui/textarea";
import { errorMessage, queryClient } from "@/lib/api";
import { offReason, parseHosts } from "@/lib/federation";

/**
 * Settings → Federation (superadmin, docs/M7E_DESIGN.md §2): off, only
 * the servers listed, or open but for those refused; this server's
 * address and signing key; and the federation settings of the levels.
 */
export const Route = createFileRoute("/_app/settings/federation")({
  component: () => (
    <SuperadminGate>
      <Federation />
      <LevelSettingsCard
        scope={{ level: "superadmin" }}
        prefixes={["federation."]}
        title="Sharing with other servers"
        description="Whether drives may be shared with people on other servers (teams and drives may forbid it), with any address or contacts only, and whether search and the AI read what other servers share."
      />
    </SuperadminGate>
  ),
});

const MODES: readonly { value: FederationMode; label: string; detail: string }[] = [
  {
    value: "off",
    label: "Off",
    detail: "Nothing is shared with other servers, and they can't find this one.",
  },
  {
    value: "listed",
    label: "Only these servers",
    detail: "Their people can be shared with without being contacts first.",
  },
  { value: "open", label: "Open", detail: "Any server, except those you refuse." },
];

type Lists = { allowed: string[]; refused: string[] };

function listsOf(settings: FederationSettings): Lists {
  return {
    allowed: settings.allowed.map((s) => s.host),
    refused: settings.refused.map((s) => s.host),
  };
}

function Federation() {
  const settings = useQuery(getFederationOptions());
  const [editing, setEditing] = useState<"allowed" | "refused">();
  const save = useMutation({
    ...setFederationMutation(),
    onSuccess: (saved) => {
      queryClient.setQueryData(getFederationOptions().queryKey, saved);
      toast.success("Saved");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const data = settings.data;
  const put = (mode: FederationMode, lists: Lists) =>
    save.mutateAsync({ body: { mode, ...lists } });
  const why = data && offReason(data);
  return (
    <SettingsCard
      title="Federation"
      description={
        data?.host ? (
          <>
            Share with people on other servers through Open Cloud Mesh. This server is{" "}
            <span className="font-mono">{data.host}</span>.
          </>
        ) : (
          "Share with people on other servers through Open Cloud Mesh."
        )
      }
      enabled={data ? data.effective_mode !== "off" : undefined}
      error={settings.error}
    >
      {data && (
        <div className="flex flex-col gap-4">
          {why && (
            <Alert>
              <AlertDescription>{why}</AlertDescription>
            </Alert>
          )}
          <RadioGroup
            value={data.mode}
            disabled={save.isPending}
            onValueChange={(mode) => void put(mode as FederationMode, listsOf(data))}
          >
            {MODES.map((mode) => {
              const list =
                mode.value === "listed"
                  ? data.allowed
                  : mode.value === "open"
                    ? data.refused
                    : undefined;
              return (
                <div key={mode.value} className="flex items-start gap-2">
                  <Label className="flex-1 items-start font-normal">
                    <RadioGroupItem value={mode.value} />
                    <span className="flex min-w-0 flex-col gap-1">
                      <span className="font-medium">{mode.label}</span>
                      <span className="text-muted-foreground">{mode.detail}</span>
                      {list && (
                        <span className="text-muted-foreground">
                          {list.length
                            ? `${mode.value === "open" ? "Refused: " : ""}${list.map((s) => s.host).join(" · ")}`
                            : mode.value === "open"
                              ? "None refused."
                              : "No server listed yet."}
                        </span>
                      )}
                    </span>
                  </Label>
                  {list && (
                    <Button
                      size="sm"
                      variant="outline"
                      onClick={() => setEditing(mode.value === "open" ? "refused" : "allowed")}
                    >
                      <PencilIcon />
                      Edit list
                    </Button>
                  )}
                </div>
              );
            })}
          </RadioGroup>
          {data.key_id && (
            <p className="text-sm text-muted-foreground">
              Signing key <span className="font-mono break-all">{data.key_id}</span>
              {data.discovery_url && (
                <>
                  {" "}
                  · discovery at <span className="font-mono break-all">{data.discovery_url}</span>
                </>
              )}
            </p>
          )}
        </div>
      )}
      {editing && data && (
        <ListDialog
          which={editing}
          hosts={listsOf(data)[editing]}
          pending={save.isPending}
          error={save.error}
          onSave={(hosts) =>
            void put(data.mode, { ...listsOf(data), [editing]: hosts }).then(() =>
              setEditing(undefined),
            )
          }
          onClose={() => setEditing(undefined)}
        />
      )}
    </SettingsCard>
  );
}

/** "Edit list": the servers listed (or refused), one host per line. */
function ListDialog({
  which,
  hosts,
  pending,
  error,
  onSave,
  onClose,
}: {
  which: "allowed" | "refused";
  hosts: string[];
  pending: boolean;
  error: unknown;
  onSave: (hosts: string[]) => void;
  onClose: () => void;
}) {
  const [text, setText] = useState(hosts.join("\n"));
  return (
    <FormDialog
      title={which === "allowed" ? "Servers to share with" : "Servers refused"}
      description={
        which === "allowed"
          ? "Only these servers are federated with while the mode is “Only these servers”."
          : "While federation is open, these servers are refused: their shares stop."
      }
      action="Save"
      pending={pending}
      error={error}
      onSubmit={() => onSave(parseHosts(text))}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="federation-hosts">Servers</FieldLabel>
        <Textarea
          id="federation-hosts"
          rows={6}
          placeholder={"cloud.partner.example\nnextcloud.uni.example"}
          value={text}
          onChange={(e) => setText(e.target.value)}
          autoFocus
        />
        <FieldDescription>One host per line, with :port when not the default.</FieldDescription>
      </Field>
    </FormDialog>
  );
}
