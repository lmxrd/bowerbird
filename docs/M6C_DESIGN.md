# M6c design proposal — reading images, scans and audio

Status: **accepted 2026-11-08** (§6); **built 2026-11-08** (§7). The third
step of M6 ([M6_OUTLINE.md](M6_OUTLINE.md)): photos, scans and recordings
get text, so the drop box can place them by what they are and search can
find them. Decided: **the admin chooses how** in the settings.

## 1. Models, a default, and overrides per kind (decided)

The AI connection becomes a **list of models** instead of one provider with
one chat and one embeddings model:

- **Register a model**: a name the admin gives it, its provider (the genai
  adapters, or an OpenAI-compatible endpoint), its endpoint, API key and
  the provider's model name, and what it can do: **chat**, **vision**
  (images), **speech** (transcription), **embeddings**. Test it on save, as
  now.
- **A default model** (a chat model) does everything a chat model can.
- **Overrides per kind of work**, each "the default" or a registered model
  that can do it:

  | Work | Needs | Today |
  | --- | --- | --- |
  | Drop box proposals | chat | the one model |
  | Embeddings (semantic search, retrieval) | embeddings | the embeddings model |
  | Photos and images | vision | — (off) |
  | Scanned PDFs | vision | — (off) |
  | Audio | speech | — (off) |

  Images, scans and audio are **off** until a model is chosen for them.
- Each registered model shows where its requests go, as the "What
  Organizer sends to the AI" description does today.
- **Later, users** may register their own models and choose them for
  their own drop box and files; the tables carry an owner for that (none
  for the admin's), but M6c builds the admin's part only.

**Variables**: the `AI_*` variables keep working and define one locked
model, "from the environment", used as the default (with
`AI_EMBEDDINGS_MODEL` as a locked embeddings model). New ones lock the
overrides: `AI_MODEL_IMAGES`, `AI_MODEL_SCANS`, `AI_MODEL_AUDIO`,
`AI_MODEL_EMBEDDINGS` (by the registered model's name).

**Migration**: the current connection becomes two registered models (the
chat model as the default, the embeddings model as the embeddings
override), so nothing changes for a running server.

Everything else stays: a user's "read contents" setting and the
sensitive-file rules decide whether a file's contents are read at all; a
file the guard holds by name is never read or sent.

## 2. Built-in OCR: not now

xberg's OCR compiles Tesseract and Leptonica (C++) from sources it
downloads during the build, which does not build into the static binary
(tried 2026-11-08). Reading images and scans is through a vision model;
a built-in reader can come later if a pure Rust one appears.

## 3. A vision model

A registered model with vision (e.g. `qwen2.5vl`, `llava`,
`gpt-4o-mini`): the image is sent as it is (up to 20 MB; larger ones are
placed by name, so no image library is added to resize them) with a
request for its text and a one-line description. Scanned PDF pages go as
the page images xberg finds in them. That text is what the drop box
and search see, marked as read by the model.

## 4. A speech model

A registered model with speech: an OpenAI-compatible transcription
endpoint (`/audio/transcriptions`: OpenAI, a local whisper.cpp or
faster-whisper server) and its model (e.g. `whisper-1`). Recordings longer than an hour are
read for their first hour.

## 5. When

Reading happens where text is read now: the drop box's analysis (before
the proposal) and the search index's text refresh, both as jobs, at the AI
connection's concurrency. A file is read once per content (its hash), so
moving or renaming does not read it again; changing the setting reads what
is new from then on, and "Read existing files again" in the card queues
the rest.

## 6. Decided (2026-11-08)

1. **Models only** for images, scans and audio for now (no built-in OCR).
2. **A registry of models** with a default and overrides per kind of work;
   users' own models later.
3. **The AI settings page** (Models, Work, the Add a model dialog):
   approved as mocked up.

## 7. As built

- **Registry**: `ai_models` (owner NULL for the admin's) and the choices in
  `ai_settings` (migration 0020, which registers the old connection); the
  environment's models are `env` and `env-embeddings`. Concurrency and
  timeout stay one setting for all the models (`AI_CONCURRENCY`,
  `AI_TIMEOUT`), on the Work card.
- **Images**: JPEG, PNG, WebP and GIF (what vision models take); HEIC and
  others are placed by name.
- **Scans**: a PDF whose text xberg finds empty; its first 20 page images.
- **Audio**: at most 100 MB is sent; the first hour is the transcription's
  segments that start before 3600 s (`verbose_json`), since no audio
  library cuts the file itself. A server that gives no segments has its
  whole text kept.
- **Read once**: `model_reads`, by user, content hash and work.
- **Keyword rules** can only look at what a model read: the image or
  recording has been sent by then. Name and path rules hold a file back
  before anything is read.

