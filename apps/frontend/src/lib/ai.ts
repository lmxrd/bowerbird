import type {
  AiLock,
  AiModel,
  AiModelChoice,
  AiModelChoices,
  AiModelInput,
  AiSettings,
  Capability,
  UpdateAiSettings,
} from "organizer-sdk/types";

/** What a model can do, in the order shown. */
export const CAPABILITIES: readonly Capability[] = ["chat", "vision", "speech", "embeddings"];

/** The kinds of work, in the order shown, with what their model must do. */
export const WORKS = [
  {
    key: "proposals",
    label: "Drop box proposals",
    description: "Where dropped items go, and their names.",
    needs: "chat",
  },
  {
    key: "embeddings",
    label: "Embeddings",
    description: "Semantic search and better proposals.",
    needs: "embeddings",
  },
  {
    key: "images",
    label: "Photos and images",
    description: "Their text and what they show, read by a vision model.",
    needs: "vision",
  },
  {
    key: "scans",
    label: "Scanned PDFs",
    description: "The pages of PDFs without text, read by a vision model.",
    needs: "vision",
  },
  {
    key: "audio",
    label: "Audio",
    description: "Recordings transcribed by a speech model (their first hour).",
    needs: "speech",
  },
] as const satisfies readonly {
  key: string;
  label: string;
  description: string;
  needs: Capability;
}[];

export type WorkKey = (typeof WORKS)[number]["key"];

const providerNames: Record<string, string> = {
  openai: "OpenAI-compatible (Ollama, llama.cpp, LM Studio, vLLM, OpenAI)",
  ollama: "Ollama",
  anthropic: "Anthropic",
  gemini: "Gemini",
  groq: "Groq",
  xai: "xAI",
  deepseek: "DeepSeek",
  cohere: "Cohere",
};

/** A provider's name as people know it. */
export function providerName(provider: string): string {
  return providerNames[provider] ?? provider.charAt(0).toUpperCase() + provider.slice(1);
}

/** A model's line under its name: "provider · endpoint · model". */
export function modelDetail(model: AiModel): string {
  const provider = model.provider === "openai" ? "OpenAI-compatible" : providerName(model.provider);
  return [provider, model.sends_to || model.endpoint || "its own endpoint", model.model].join(
    " · ",
  );
}

/** The choices of a work's select: its value ("" for the default or off) and label. */
export function workOptions(
  settings: Pick<AiSettings, "models" | "default_model">,
  work: (typeof WORKS)[number],
): { value: string; label: string }[] {
  const able = settings.models
    .filter((m) => m.capabilities.includes(work.needs))
    .map((m) => ({ value: m.id, label: m.name }));
  if (work.key === "proposals") {
    const fallback = settings.models.find((m) => m.id === settings.default_model);
    return [{ value: "", label: `The default (${fallback?.name ?? "none"})` }, ...able];
  }
  return [{ value: "", label: "Off" }, ...able];
}

/** The variable that sets a choice, with the name it gives: "AI_MODEL_IMAGES (Eyes)". */
export function lockedBy(locked: readonly AiLock[], key: string): string | undefined {
  const lock = locked.find((l) => l.key === key);
  if (!lock) return undefined;
  return lock.value ? `${lock.variable} (${lock.value})` : lock.variable;
}

/** The Work card's form: each work's model ("" for the default or off) and the limits. */
export type WorkForm = Record<WorkKey, string> & { concurrency: string; timeout_seconds: string };

export function workForm(settings: AiSettings): WorkForm {
  return {
    proposals: settings.work.proposals ?? "",
    embeddings: settings.work.embeddings ?? "",
    images: settings.work.images ?? "",
    scans: settings.work.scans ?? "",
    audio: settings.work.audio ?? "",
    concurrency: String(settings.concurrency),
    timeout_seconds: String(settings.timeout_seconds),
  };
}

/** What the Work card's Save sends: the changed choices that no variable sets. */
export function workChanges(form: WorkForm, settings: AiSettings): UpdateAiSettings {
  const loaded = workForm(settings);
  const body: UpdateAiSettings = {};
  for (const { key } of WORKS) {
    if (form[key] !== loaded[key] && !lockedBy(settings.locked, key)) body[key] = form[key];
  }
  for (const key of ["concurrency", "timeout_seconds"] as const) {
    if (form[key] !== loaded[key] && !lockedBy(settings.locked, key)) body[key] = Number(form[key]);
  }
  return body;
}

/** The Add a model dialog's form. */
export type ModelForm = {
  name: string;
  provider: string;
  endpoint: string;
  model: string;
  capabilities: Capability[];
};

export function modelForm(model?: AiModel): ModelForm {
  return {
    name: model?.name ?? "",
    provider: model?.provider ?? "openai",
    endpoint: model?.endpoint ?? "",
    model: model?.model ?? "",
    capabilities: model ? [...model.capabilities] : ["chat"],
  };
}

/**
 * What the dialog sends: every field for a new model, the changed ones for a
 * registered one; the key only when one was typed (or cleared), so that it
 * is otherwise kept.
 */
export function modelInput(
  form: ModelForm,
  apiKey: string | undefined,
  model?: AiModel,
): AiModelInput {
  const trimmed = {
    name: form.name.trim(),
    provider: form.provider,
    endpoint: form.endpoint.trim(),
    model: form.model.trim(),
  };
  const body: AiModelInput = {};
  for (const key of ["name", "provider", "endpoint", "model"] as const) {
    if (!model || trimmed[key] !== model[key]) body[key] = trimmed[key];
  }
  const capabilities = CAPABILITIES.filter((c) => form.capabilities.includes(c));
  if (!model || capabilities.join() !== model.capabilities.join()) body.capabilities = capabilities;
  if (apiKey !== undefined) body.api_key = apiKey;
  return body;
}

/** A capability turned on or off in a list, in the order shown. */
export function toggle(
  list: readonly Capability[],
  capability: Capability,
  on: boolean,
): Capability[] {
  return CAPABILITIES.filter((c) => (c === capability ? on : list.includes(c)));
}

/**
 * The models one may choose for a setting `ai.model.<work>` (from
 * `listSelectableAiModels`); none for another key or a work without a
 * user's choice.
 */
export function modelChoicesFor(key: string, choices: AiModelChoices | undefined): AiModelChoice[] {
  if (!choices || !key.startsWith("ai.model.")) return [];
  const work = key.slice("ai.model.".length);
  return work in choices ? choices[work as keyof AiModelChoices] : [];
}

/**
 * The items of a model's select: "The AI page's choice" (the value ""),
 * the models one may choose, and a chosen one no longer among them (removed
 * since, or no longer able), by its id.
 */
export function modelItems(
  models: AiModelChoice[],
  value: unknown,
): { value: string; label: string }[] {
  return [
    { value: "", label: "The AI page's choice" },
    ...models.map((m) => ({ value: m.id, label: m.name })),
    ...(typeof value === "string" && value && !models.some((m) => m.id === value)
      ? [{ value, label: value }]
      : []),
  ];
}
