import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import {
  BotIcon,
  FlaskConicalIcon,
  LockIcon,
  PencilIcon,
  StarIcon,
  Trash2Icon,
} from "lucide-react";
import { toast } from "sonner";
import type { AiModel, AiSettings } from "organizer-sdk/types";
import {
  addAiModelMutation,
  getAiSettingsOptions,
  readFilesAgainMutation,
  removeAiModelMutation,
  testAiModelMutation,
  updateAiModelMutation,
  updateAiSettingsMutation,
} from "organizer-sdk/react-query";
import { AiRecipient } from "@/components/ai-recipient";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { FormDialog } from "@/components/form-dialog";
import { LevelSettingsCard } from "@/components/levels";
import { ListRow } from "@/components/list-row";
import { RowMenu } from "@/components/row-menu";
import { SuperadminGate } from "@/components/step-up";
import {
  FormOutcome,
  SecretInput,
  SelectField,
  SettingField,
  SettingsCard,
  SwitchField,
} from "@/components/settings";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { FieldGroup, FieldLegend, FieldSet } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  CAPABILITIES,
  lockedBy,
  modelDetail,
  modelForm,
  modelInput,
  providerName,
  toggle,
  WORKS,
  workChanges,
  workForm,
  workOptions,
  type ModelForm,
  type WorkForm,
} from "@/lib/ai";
import { errorMessage, queryClient } from "@/lib/api";

// A superadmin page: a step-up first (`SuperadminGate`).
export const Route = createFileRoute("/_app/settings/ai")({
  component: () => (
    <SuperadminGate>
      <AiSettingsPage />
    </SuperadminGate>
  ),
});

/** What a test found, said in a sentence. */
function testedMessage(result: { embedding_dimensions?: number | null }): string {
  const dimensions = result.embedding_dimensions;
  return `The model answered${dimensions ? `, embeddings: ${dimensions} dimensions` : ""}.`;
}

function AiSettingsPage() {
  const settings = useQuery(getAiSettingsOptions());
  const data = settings.data;
  return (
    <>
      <ModelsCard settings={data} error={settings.error} />
      {data && <WorkCard settings={data} />}
      <LevelSettingsCard
        scope={{ level: "superadmin" }}
        prefixes={["ai."]}
        title="What users choose"
        description="Each kind of work's model for users and teams: the Work card's choice until set here, a default they may change, or locked."
      />
      {data && <AiRecipient recipient={data.recipient} />}
    </>
  );
}

/** The registered models (and the environment's), with what each can do. */
function ModelsCard({ settings, error }: { settings?: AiSettings; error: unknown }) {
  // The dialog: a new model (null) or one to edit.
  const [editing, setEditing] = useState<AiModel | null>();
  const [removing, setRemoving] = useState<AiModel>();
  const done = (message: string) => () => {
    void queryClient.invalidateQueries();
    toast.success(message);
  };
  const failed = (error: unknown) => toast.error(errorMessage(error));
  const makeDefault = useMutation({
    ...updateAiSettingsMutation(),
    onSuccess: done("Default model changed"),
    onError: failed,
  });
  const test = useMutation({
    ...testAiModelMutation(),
    onSuccess: (result) => toast.success(testedMessage(result)),
    onError: failed,
  });
  const remove = useMutation({
    ...removeAiModelMutation(),
    onSuccess: done("Model removed"),
    onError: failed,
  });
  const defaultLock = settings && lockedBy(settings.locked, "default");
  return (
    <SettingsCard
      title="Models"
      description="The models Organizer can use, local or in the cloud. Any OpenAI-compatible server works, your own included."
      enabled={settings?.enabled}
      error={error}
      action={
        settings && (
          <Button size="sm" variant="outline" onClick={() => setEditing(null)}>
            Add a model
          </Button>
        )
      }
    >
      {settings && settings.models.length === 0 && (
        <p className="text-sm text-muted-foreground">
          No model yet: drop box items wait until one is added, and can be placed by hand.
        </p>
      )}
      <div className="flex flex-col divide-y">
        {settings?.models.map((model) => (
          <ListRow
            key={model.id}
            icon={
              model.from_environment ? (
                <LockIcon className="size-4 shrink-0 text-muted-foreground" />
              ) : (
                <BotIcon className="size-4 shrink-0 text-muted-foreground" />
              )
            }
            title={
              <span className="flex items-center gap-2">
                {model.name}
                {model.id === settings.default_model && <Badge>Default</Badge>}
              </span>
            }
            detail={
              model.from_environment
                ? `${modelDetail(model)} · set by ${model.variables.join(", ")}`
                : modelDetail(model)
            }
          >
            <div className="hidden gap-1 sm:flex">
              {model.capabilities.map((capability) => (
                <Badge key={capability} variant="secondary">
                  {capability}
                </Badge>
              ))}
            </div>
            <RowMenu
              label={`Actions for ${model.name}`}
              actions={[
                ...(model.from_environment
                  ? []
                  : [
                      {
                        label: "Make default",
                        icon: <StarIcon />,
                        disabled:
                          !model.capabilities.includes("chat") ||
                          model.id === settings.default_model ||
                          defaultLock !== undefined,
                        onClick: () => makeDefault.mutate({ body: { default_model: model.id } }),
                      },
                      { label: "Edit", icon: <PencilIcon />, onClick: () => setEditing(model) },
                    ]),
                {
                  label: "Test",
                  icon: <FlaskConicalIcon />,
                  disabled: test.isPending,
                  onClick: () => test.mutate({ body: { id: model.id } }),
                },
                ...(model.from_environment
                  ? []
                  : [
                      {
                        label: "Remove",
                        icon: <Trash2Icon />,
                        destructive: true,
                        onClick: () => setRemoving(model),
                      },
                    ]),
              ]}
            />
          </ListRow>
        ))}
      </div>
      {settings && defaultLock && (
        <p className="mt-2 flex items-center gap-1 text-sm text-muted-foreground">
          <LockIcon className="size-3" /> The default is set by {defaultLock}.
        </p>
      )}
      {settings && editing !== undefined && (
        <ModelDialog
          model={editing ?? undefined}
          providers={settings.providers}
          onClose={() => setEditing(undefined)}
        />
      )}
      {removing && (
        <ConfirmDialog
          title={`Remove ${removing.name}?`}
          description="Its work goes back to the default model, or off."
          action="Remove"
          onConfirm={() => remove.mutate({ path: { id: removing.id } })}
          onClose={() => setRemoving(undefined)}
        />
      )}
    </SettingsCard>
  );
}

/** Add a model, or edit one: tested before it is saved. */
function ModelDialog({
  model,
  providers,
  onClose,
}: {
  model?: AiModel;
  providers: string[];
  onClose: () => void;
}) {
  const [form, setForm] = useState<ModelForm>(modelForm(model));
  // Undefined: the key is left as it is; a string (even empty) replaces it.
  const [apiKey, setApiKey] = useState<string>();
  const [tested, setTested] = useState<string>();
  const saved = () => {
    void queryClient.invalidateQueries();
    toast.success(model ? "Saved" : "Model added");
    onClose();
  };
  const add = useMutation({ ...addAiModelMutation(), onSuccess: saved });
  const update = useMutation({ ...updateAiModelMutation(), onSuccess: saved });
  const save = model ? update : add;
  const test = useMutation({
    ...testAiModelMutation(),
    onMutate: () => setTested(undefined),
    onSuccess: (result) => setTested(testedMessage(result)),
  });
  const body = () => modelInput(form, apiKey, model);
  const set = (changes: Partial<ModelForm>) => setForm({ ...form, ...changes });
  return (
    <FormDialog
      title={model ? `Edit ${model.name}` : "Add a model"}
      description="It is tested before it is saved, for each thing it can do."
      action={model ? "Save" : "Add"}
      pending={save.isPending}
      onSubmit={() => {
        test.reset();
        setTested(undefined);
        if (model) update.mutate({ path: { id: model.id }, body: body() });
        else add.mutate({ body: body() });
      }}
      onClose={onClose}
      extra={
        <Button
          type="button"
          variant="outline"
          disabled={test.isPending}
          onClick={() => {
            save.reset();
            test.mutate({ body: { ...body(), id: model?.id } });
          }}
        >
          {test.isPending ? "Testing…" : "Test"}
        </Button>
      }
    >
      <SettingField id="model-name" label="Name">
        <Input
          id="model-name"
          placeholder="Local Qwen"
          value={form.name}
          onChange={(e) => set({ name: e.target.value })}
        />
      </SettingField>
      <SelectField
        id="model-provider"
        label="Provider"
        placeholder="Choose a provider"
        items={providers.map((p) => ({ value: p, label: providerName(p) }))}
        value={form.provider}
        onChange={(provider) => set({ provider })}
      />
      <SettingField
        id="model-endpoint"
        label="Endpoint"
        description="Empty for the provider's own."
      >
        <Input
          id="model-endpoint"
          placeholder="http://localhost:11434/v1"
          value={form.endpoint}
          onChange={(e) => set({ endpoint: e.target.value })}
        />
      </SettingField>
      <SettingField
        id="model-key"
        label="API key"
        description={
          model?.api_key_set && "Type something, clear it again and save to remove the key."
        }
      >
        <SecretInput
          id="model-key"
          set={model?.api_key_set ?? false}
          value={apiKey ?? ""}
          onChange={setApiKey}
        />
      </SettingField>
      <SettingField id="model-model" label="Model" description="The provider's name of the model.">
        <Input
          id="model-model"
          placeholder="qwen3"
          value={form.model}
          onChange={(e) => set({ model: e.target.value })}
        />
      </SettingField>
      <FieldSet>
        <FieldLegend variant="label">Can do</FieldLegend>
        {CAPABILITIES.map((capability) => (
          <SwitchField
            key={capability}
            id={`model-${capability}`}
            label={capabilityLabels[capability]}
            checked={form.capabilities.includes(capability)}
            onChange={(on) => set({ capabilities: toggle(form.capabilities, capability, on) })}
          />
        ))}
      </FieldSet>
      <FormOutcome
        tested={tested}
        testError={test.isError ? test.error : undefined}
        saveError={save.isError ? save.error : undefined}
      />
    </FormDialog>
  );
}

const capabilityLabels = {
  chat: "Chat: drop box proposals",
  vision: "Vision: photos and scanned pages",
  speech: "Speech: transcribing recordings",
  embeddings: "Embeddings: semantic search",
} as const;

/** Which model does which work. */
function WorkCard({ settings }: { settings: AiSettings }) {
  const [form, setForm] = useState<WorkForm>(workForm(settings));
  useEffect(() => setForm(workForm(settings)), [settings]);
  const save = useMutation({
    ...updateAiSettingsMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Saved");
    },
  });
  const readAgain = useMutation({
    ...readFilesAgainMutation(),
    onSuccess: ({ queued }) =>
      toast.success(
        queued === 0
          ? "Nothing is left to read."
          : `${queued} ${queued === 1 ? "file is" : "files are"} read again.`,
      ),
    onError: (error) => toast.error(errorMessage(error)),
  });
  const limits = [
    { key: "concurrency", label: "Requests at once" },
    { key: "timeout_seconds", label: "Timeout (seconds)" },
  ] as const;
  return (
    <SettingsCard
      title="Work"
      description="Which model does what. Photos, scans and audio are read once a model is chosen for them, from then on."
    >
      <form
        onSubmit={(e) => {
          e.preventDefault();
          save.mutate({ body: workChanges(form, settings) });
        }}
      >
        <FieldGroup>
          {WORKS.map((work) => (
            <SelectField
              key={work.key}
              id={`work-${work.key}`}
              label={work.label}
              description={work.description}
              lockedBy={lockedBy(settings.locked, work.key)}
              items={workOptions(settings, work)}
              value={form[work.key]}
              onChange={(value) => setForm({ ...form, [work.key]: value })}
            />
          ))}
          <div className="grid gap-4 sm:grid-cols-2">
            {limits.map(({ key, label }) => (
              <SettingField
                key={key}
                id={key}
                label={label}
                lockedBy={lockedBy(settings.locked, key)}
              >
                <Input
                  id={key}
                  type="number"
                  min={1}
                  value={form[key]}
                  disabled={lockedBy(settings.locked, key) !== undefined}
                  onChange={(e) => setForm({ ...form, [key]: e.target.value })}
                />
              </SettingField>
            ))}
          </div>
          <FormOutcome saveError={save.isError ? save.error : undefined} />
          <div className="flex flex-wrap gap-2">
            <Button
              type="button"
              variant="outline"
              disabled={readAgain.isPending}
              onClick={() => readAgain.mutate({})}
            >
              Read existing files again
            </Button>
            <Button type="submit" disabled={save.isPending}>
              Save
            </Button>
          </div>
        </FieldGroup>
      </form>
    </SettingsCard>
  );
}
