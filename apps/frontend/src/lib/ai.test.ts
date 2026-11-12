import { describe, expect, it } from "vitest";
import type { AiModel, AiSettings } from "organizer-sdk/types";
import {
  lockedBy,
  modelDetail,
  modelForm,
  modelChoicesFor,
  modelInput,
  modelItems,
  providerName,
  toggle,
  WORKS,
  workChanges,
  workForm,
  workOptions,
} from "./ai";

const model = (id: string, name: string, capabilities: AiModel["capabilities"]): AiModel => ({
  id,
  name,
  provider: "openai",
  endpoint: "http://gpu:11434/v1",
  api_key_set: false,
  model: name.toLowerCase(),
  capabilities,
  from_environment: false,
  variables: [],
  sends_to: "http://gpu:11434/v1/",
});

const local = model("1", "Local", ["chat", "embeddings"]);
const eyes = model("2", "Eyes", ["vision"]);
const ears = model("3", "Ears", ["speech"]);

const settings: AiSettings = {
  enabled: true,
  models: [local, eyes, ears],
  default_model: "1",
  work: { proposals: null, embeddings: "1", images: null, scans: null, audio: "3" },
  concurrency: 1,
  timeout_seconds: 120,
  locked: [{ key: "images", variable: "AI_MODEL_IMAGES", value: "Eyes" }],
  providers: ["openai", "ollama"],
  recipient: {
    enabled: true,
    provider: "openai",
    endpoint: "",
    model: "local",
    embeddings_model: "",
    embeddings_endpoint: "",
  },
};

const work = (key: string) => WORKS.find((w) => w.key === key)!;

describe("ai", () => {
  it("names providers and describes models", () => {
    expect(providerName("ollama")).toBe("Ollama");
    expect(providerName("mistral")).toBe("Mistral");
    expect(modelDetail(local)).toBe("OpenAI-compatible · http://gpu:11434/v1/ · local");
    expect(modelDetail({ ...local, provider: "anthropic", sends_to: "", endpoint: "" })).toBe(
      "Anthropic · its own endpoint · local",
    );
  });

  it("offers each work the models that can do it", () => {
    expect(workOptions(settings, work("proposals"))).toEqual([
      { value: "", label: "The default (Local)" },
      { value: "1", label: "Local" },
    ]);
    expect(workOptions(settings, work("images"))).toEqual([
      { value: "", label: "Off" },
      { value: "2", label: "Eyes" },
    ]);
    expect(workOptions(settings, work("audio")).map((o) => o.label)).toEqual(["Off", "Ears"]);
    expect(workOptions({ models: [], default_model: null }, work("proposals"))[0].label).toBe(
      "The default (none)",
    );
  });

  it("says which variable locks a choice", () => {
    expect(lockedBy(settings.locked, "images")).toBe("AI_MODEL_IMAGES (Eyes)");
    expect(lockedBy([{ key: "default", variable: "AI_MODEL" }], "default")).toBe("AI_MODEL");
    expect(lockedBy(settings.locked, "audio")).toBeUndefined();
  });

  it("sends the changed choices that no variable sets", () => {
    const form = workForm(settings);
    expect(form.embeddings).toBe("1");
    expect(form.proposals).toBe("");
    expect(workChanges(form, settings)).toEqual({});
    expect(
      workChanges({ ...form, audio: "", images: "", scans: "2", concurrency: "3" }, settings),
    ).toEqual({ audio: "", scans: "2", concurrency: 3 });
  });

  it("sends a new model whole, a registered one's changes, the key when typed", () => {
    const fresh = {
      ...modelForm(),
      name: " Eyes ",
      model: "llava",
      capabilities: toggle(["chat"], "vision", true),
    };
    expect(modelInput(fresh, undefined)).toEqual({
      name: "Eyes",
      provider: "openai",
      endpoint: "",
      model: "llava",
      capabilities: ["chat", "vision"],
    });
    expect(modelInput(modelForm(local), undefined, local)).toEqual({});
    expect(
      modelInput(
        { ...modelForm(local), capabilities: toggle(local.capabilities, "embeddings", false) },
        "",
        local,
      ),
    ).toEqual({ capabilities: ["chat"], api_key: "" });
  });
});

describe("modelChoicesFor", () => {
  const choices = {
    proposals: [{ id: "1", name: "Local" }],
    images: [{ id: "2", name: "Eyes" }],
    scans: [],
    audio: [],
  };
  it("gives the models of a setting's work", () => {
    expect(modelChoicesFor("ai.model.images", choices)).toEqual([{ id: "2", name: "Eyes" }]);
    expect(modelChoicesFor("ai.model.proposals", choices)).toEqual([{ id: "1", name: "Local" }]);
  });
  it("gives none for another setting, an unknown work or no answer yet", () => {
    expect(modelChoicesFor("links.allowed", choices)).toEqual([]);
    expect(modelChoicesFor("ai.model.embeddings", choices)).toEqual([]);
    expect(modelChoicesFor("ai.model.images", undefined)).toEqual([]);
  });
});

describe("modelItems", () => {
  const models = [{ id: "2", name: "Eyes" }];
  it("starts with the AI page's choice, then the models", () => {
    expect(modelItems(models, null)).toEqual([
      { value: "", label: "The AI page's choice" },
      { value: "2", label: "Eyes" },
    ]);
  });
  it("keeps a chosen model that is no longer listed, by its id", () => {
    expect(modelItems(models, "9").at(-1)).toEqual({ value: "9", label: "9" });
    expect(modelItems(models, "2")).toHaveLength(2);
  });
});
