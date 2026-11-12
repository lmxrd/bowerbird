import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { toast } from "sonner";
import type { UpdateOidcSettings } from "organizer-sdk";
import { getOidcSettingsOptions, updateOidcSettingsMutation } from "organizer-sdk/react-query";
import { Button } from "@/components/ui/button";
import { FormOutcome, SecretInput, SettingField, SettingsCard } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { FieldDescription, FieldGroup } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { queryClient } from "@/lib/api";

// A superadmin page: a step-up first (`SuperadminGate`).
export const Route = createFileRoute("/_app/settings/sso")({
  component: () => (
    <SuperadminGate>
      <SingleSignOn />
    </SuperadminGate>
  ),
});

const fields = [
  { key: "issuer", label: "Issuer", placeholder: "https://id.example.com/realms/main" },
  { key: "client_id", label: "Client id", placeholder: "organizer" },
  { key: "scopes", label: "Scopes", placeholder: "openid email" },
  { key: "label", label: "Button label", placeholder: "Single sign-on" },
  { key: "redirect_url", label: "Callback URL (optional)", placeholder: "" },
] as const;

function SingleSignOn() {
  const settings = useQuery(getOidcSettingsOptions());
  const [form, setForm] = useState<Record<string, string>>({});
  const [secret, setSecret] = useState("");
  useEffect(() => {
    if (settings.data) {
      setForm(Object.fromEntries(fields.map((f) => [f.key, settings.data[f.key]])));
    }
  }, [settings.data]);
  const save = useMutation({
    ...updateOidcSettingsMutation(),
    onSuccess: () => {
      setSecret("");
      void queryClient.invalidateQueries();
      toast.success("Saved");
    },
  });
  const data = settings.data;
  const locked = (key: string) => data?.locked.includes(key) ?? false;
  return (
    <SettingsCard
      title="Single sign-on"
      description="OpenID Connect. Organizer asks the provider before it saves. Someone signing in for the first time is matched to their account by a verified email."
      enabled={data?.enabled}
      error={settings.error}
    >
      {data && (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const body: UpdateOidcSettings = {};
            for (const f of fields) {
              if (!locked(f.key)) body[f.key] = form[f.key] ?? "";
            }
            if (secret && !locked("client_secret")) body.client_secret = secret;
            save.mutate({ body });
          }}
        >
          <FieldGroup>
            {fields.map((f) => (
              <SettingField
                key={f.key}
                id={f.key}
                label={f.label}
                lockedBy={locked(f.key) ? `OIDC_${f.key.toUpperCase()}` : undefined}
              >
                <Input
                  id={f.key}
                  placeholder={f.placeholder}
                  value={form[f.key] ?? ""}
                  disabled={locked(f.key)}
                  onChange={(e) => setForm({ ...form, [f.key]: e.target.value })}
                />
              </SettingField>
            ))}
            <SettingField
              id="secret"
              label="Client secret"
              lockedBy={locked("client_secret") ? "OIDC_CLIENT_SECRET" : undefined}
            >
              <SecretInput
                id="secret"
                set={data.client_secret_set}
                value={secret}
                disabled={locked("client_secret")}
                onChange={setSecret}
              />
            </SettingField>
            <FieldDescription>
              Register this callback URL with the provider: {data.callback_url}
            </FieldDescription>
            <FormOutcome saveError={save.isError ? save.error : undefined} />
            <Button
              type="submit"
              variant="outline"
              className="self-start"
              disabled={save.isPending}
            >
              Save
            </Button>
          </FieldGroup>
        </form>
      )}
    </SettingsCard>
  );
}
