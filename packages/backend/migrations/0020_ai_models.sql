-- The AI connection becomes a registry of models (docs/M6C_DESIGN.md §1):
-- each registered model with what it can do, a default model, and a model
-- chosen per kind of work. The connection of `ai_settings` (one provider,
-- a chat and an embeddings model) becomes registered models, the chat model
-- the default and the embeddings model the embeddings model, so a running
-- server keeps working. The AI_* variables are one more model, locked
-- (src/ai.rs).

CREATE TABLE ai_models (
  id BLOB PRIMARY KEY,
  -- NULL: the admin's, for everyone. A user's own models are for later.
  owner_id BLOB REFERENCES users(id) ON DELETE CASCADE,
  -- The name the admin gives it; one of each per owner.
  name TEXT NOT NULL,
  -- A genai adapter: openai (any OpenAI-compatible API), ollama, anthropic,
  -- gemini, ...
  provider TEXT NOT NULL,
  -- The base URL; empty for the provider's own.
  endpoint TEXT NOT NULL DEFAULT '',
  -- Never sent back.
  api_key TEXT NOT NULL DEFAULT '',
  -- The provider's name for the model.
  model TEXT NOT NULL,
  -- What it can do.
  chat INTEGER NOT NULL DEFAULT 0,
  vision INTEGER NOT NULL DEFAULT 0,
  speech INTEGER NOT NULL DEFAULT 0,
  embeddings INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL
);
CREATE UNIQUE INDEX ai_models_name ON ai_models (ifnull(owner_id, x''), name);

-- The admin's choices: the default model and the model of each kind of
-- work. NULL: the default (proposals), or off (the rest).
ALTER TABLE ai_settings ADD COLUMN default_model BLOB REFERENCES ai_models(id) ON DELETE SET NULL;
ALTER TABLE ai_settings ADD COLUMN proposals_model BLOB REFERENCES ai_models(id) ON DELETE SET NULL;
ALTER TABLE ai_settings ADD COLUMN embeddings_model_id BLOB REFERENCES ai_models(id) ON DELETE SET NULL;
ALTER TABLE ai_settings ADD COLUMN images_model BLOB REFERENCES ai_models(id) ON DELETE SET NULL;
ALTER TABLE ai_settings ADD COLUMN scans_model BLOB REFERENCES ai_models(id) ON DELETE SET NULL;
ALTER TABLE ai_settings ADD COLUMN audio_model BLOB REFERENCES ai_models(id) ON DELETE SET NULL;

-- Today's connection, registered: the chat model...
INSERT INTO ai_models (id, name, provider, endpoint, api_key, model, chat, created_at)
SELECT randomblob(16), model, provider, endpoint, api_key, model, 1,
  strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
FROM ai_settings WHERE id = 1 AND provider != '' AND model != '';
UPDATE ai_settings SET default_model = (SELECT id FROM ai_models WHERE chat = 1)
WHERE id = 1;

-- ... and the embeddings model, on its own endpoint when it had one, with
-- the same key (as the connection sent it).
INSERT INTO ai_models (id, name, provider, endpoint, api_key, model, embeddings, created_at)
SELECT randomblob(16),
  CASE WHEN embeddings_model = model THEN embeddings_model || ' (embeddings)'
    ELSE embeddings_model END,
  provider,
  CASE WHEN embeddings_endpoint != '' THEN embeddings_endpoint ELSE endpoint END,
  api_key, embeddings_model, 1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
FROM ai_settings WHERE id = 1 AND provider != '' AND model != '' AND embeddings_model != '';
UPDATE ai_settings SET embeddings_model_id = (SELECT id FROM ai_models WHERE embeddings = 1)
WHERE id = 1;

ALTER TABLE ai_settings DROP COLUMN provider;
ALTER TABLE ai_settings DROP COLUMN endpoint;
ALTER TABLE ai_settings DROP COLUMN api_key;
ALTER TABLE ai_settings DROP COLUMN model;
ALTER TABLE ai_settings DROP COLUMN embeddings_model;
ALTER TABLE ai_settings DROP COLUMN embeddings_endpoint;
