# Prior art and similar projects

Research date: **2026-10-25**. Star counts, last-push dates and licenses come from the GitHub API on that date. The bar for inclusion was **more than 10 stars**; projects below it are only mentioned when they are unusually close to our idea.

## TL;DR

**No existing project combines what Organizer proposes**: a self-hosted file server (WebDAV, sync, mobile) whose main feature is an **AI drop box** that proposes a folder and filename and asks the user to confirm. The idea exists in pieces:

| Piece | Who already does it | What they lack |
|---|---|---|
| Self-hosted file server, WebDAV, sync, mobile | Nextcloud, Seafile, OxiCloud, OpenCloud, Cloudreve… | AI that **places** files. Seafile 13 and Nextcloud only tag or describe them. |
| AI that sorts and renames files into folders | llama-fs, Local-File-Organizer, AI File Sorter, many CLIs | All desktop or CLI tools for one machine; no server, no WebDAV, no mobile share target |
| Inbox folder → AI → move, with review | guiwei, bsnydr/organizer (both tiny), tidybot | CLI scripts, no UI, no server |
| AI suggestions with approve / auto modes, in a self-hosted app | **Paperless-ngx 3.x** (native LLM suggestions), paperless-gpt, Tagvico | Only for documents in a document system; not a general file tree |
| Share sheet → self-hosted → AI | Karakeep, Linkwarden (links and notes) | Bookmarks only, organised by tags and lists, not a file tree |
| Share sheet → AI places into folders | **Filex AI, The Drive AI** (commercial, hosted) | Not self-hosted or open source |

The closest product in spirit is the hosted, commercial **Filex AI** / **The Drive AI**. The closest open source projects are **Paperless-ngx 3.x** (for documents) and the desktop **AI File Sorter**.

---

## 1. Self-hosted file storage servers (what we compete with)

| Project | ★ | Lang | License | Last push | Notes |
|---|---:|---|---|---|---|
| [syncthing/syncthing](https://github.com/syncthing/syncthing) | 89.7k | Go | MPL-2.0 | 2026-10-24 | P2P sync only; no server UI or AI. |
| [nextcloud/server](https://github.com/nextcloud/server) | 37.3k | PHP | AGPL-3.0 | 2026-10-25 | The reference. AI via Assistant/Recognize apps (tags, chat); "Files automated tagging" is rule-based. No AI placement. |
| [filebrowser/filebrowser](https://github.com/filebrowser/filebrowser) | 35.9k | Go | Apache-2.0 | 2026-07-31 | **Archived.** Web file manager over a directory. |
| [cloudreve/cloudreve](https://github.com/cloudreve/cloudreve) | 28.9k | Go | GPL-3.0 | 2026-10-20 | Multi-backend file management and sharing, WebDAV. |
| [haiwen/seafile](https://github.com/haiwen/seafile) | 15.4k | C | custom | 2026-10-16 | Seafile 13 added a metadata server + **Seafile AI** (via LiteLLM, supports Ollama): descriptions, OCR, tags, face grouping. It does **not** move files. |
| [mickael-kerjean/filestash](https://github.com/mickael-kerjean/filestash) | 14.8k | Go | AGPL-3.0 | 2026-10-22 | File manager in front of many backends ("for humans and agents"). |
| [drakkan/sftpgo](https://github.com/drakkan/sftpgo) | 12.7k | Go | AGPL-3.0 | 2026-10-22 | SFTP/FTP/HTTP/WebDAV server. |
| [sigoden/dufs](https://github.com/sigoden/dufs) | 11.0k | **Rust** | Apache-2.0 | 2026-10-22 | Small Rust file server with WebDAV. Good reference for "light". |
| [gtsteffaniak/filebrowser](https://github.com/gtsteffaniak/filebrowser) | 8.9k | Go | Apache-2.0 | 2026-10-23 | Active fork of File Browser. |
| [owncloud/core](https://github.com/owncloud/core) | 8.8k | PHP | AGPL-3.0 | 2026-10-23 | Classic ownCloud. |
| [opencloud-eu/opencloud](https://github.com/opencloud-eu/opencloud) | 6.2k | Go | Apache-2.0 | 2026-10-24 | Fork of oCIS; "simple and sovereign". |
| [AtalayaLabs/OxiCloud](https://github.com/AtalayaLabs/OxiCloud) | 3.7k | **Rust** | MIT | 2026-10-25 | **Closest stack match**: Rust (Axum), embedded SPA, WebDAV/CalDAV/CardDAV, OIDC. Needs PostgreSQL. No AI, desktop/mobile apps "planned". |
| [pydio/cells](https://github.com/pydio/cells) | 2.3k | Go | AGPL-3.0 | 2026-10-24 | Enterprise file sharing. |
| [owncloud/ocis](https://github.com/owncloud/ocis) | 2.2k | Go | Apache-2.0 | 2026-10-25 | ownCloud Infinite Scale. |
| [Sync-in/server](https://github.com/Sync-in/server) | 1.9k | TypeScript | AGPL-3.0 | 2026-10-25 | Keeps files **plain on the filesystem** (like our plan). WebDAV, desktop + CLI sync ([Sync-in/desktop](https://github.com/Sync-in/desktop), 179★). No AI. |
| [hudikhq/hoodik](https://github.com/hudikhq/hoodik) | 1.5k | **Rust** | custom | 2026-10-22 | End-to-end encrypted storage drive. |
| [bewcloud/bewcloud](https://github.com/bewcloud/bewcloud) | 1.2k | TypeScript | AGPL-3.0 | 2026-10-18 | "Simpler Nextcloud" on Deno; has desktop and mobile apps. |
| [jamebal/jmal-cloud-view](https://github.com/jamebal/jmal-cloud-view) | 881 | JS/Java | — | 2026-08-04 | Private cloud drive (CN). |
| [HeyPuter/puter](https://github.com/HeyPuter/puter) | 43.9k | TypeScript | AGPL-3.0 | 2026-10-25 | Web "internet computer" desktop with file storage; different scope. |
| [spacedriveapp/spacedrive](https://github.com/spacedriveapp/spacedrive) | 39.3k | **Rust** | Apache-2.0 | 2026-10-13 | Cross-device file explorer over a virtual distributed filesystem; v2 alpha rewrite. Not a server/sync product, but overlaps on "organize all my files". |
| [BRF-Tech/filex](https://github.com/BRF-Tech/filex) | 70 | Go | MIT | 2026-10-21 | Single-binary file manager and sharing. |
| [nicoleroux35/HomeVault](https://github.com/nicoleroux35/HomeVault) | 11 | Rust | MIT | 2026-10-25 | Tiny-hardware home NAS. |

## 2. AI file organizers: desktop and CLI (the AI half of our idea)

| Project | ★ | Lang | License | Last push | Notes |
|---|---:|---|---|---|---|
| [iyaja/llama-fs](https://github.com/iyaja/llama-fs) | 5.8k | TS/Python | MIT | 2025-08-08 | "Self-organizing file system". Batch mode, plus a **watch mode** that learns from your recent renames/moves. Electron UI to review before applying. Groq by default, Ollama for "incognito". Quiet since 2025. |
| [QiuYannnn/Local-File-Organizer](https://github.com/QiuYannnn/Local-File-Organizer) | 3.4k | Python | MIT | 2024-10-21 | Fully local (Nexa SDK, Llama 3.2 + LLaVA). Restructures a folder in one go. Inactive. |
| [hyperfield/ai-file-sorter](https://github.com/hyperfield/ai-file-sorter) | 1.9k | C++ | AGPL-3.0 | 2026-10-19 | **The most mature desktop tool.** Category + subcategory + rename; content analysis for docs and images; **review table, dry-run, undo**; local GGUF or OpenAI-compatible; **stores approved decisions as hints** for later runs; "consistent vs refined" mode; label whitelists. |
| [ozgrozer/ai-renamer](https://github.com/ozgrozer/ai-renamer) | 2.1k | JS | GPL-3.0 | 2025-02-09 | Renames by content with Ollama / LM Studio. |
| [jjuliano/aifiles](https://github.com/jjuliano/aifiles) | 209 | TS | — | 2025-11-18 | CLI organizer. |
| [Leonard-Li777/firefly-ai-folder-desktop](https://github.com/Leonard-Li777/firefly-ai-folder-desktop) | 152 | TS | CC BY-NC-SA | 2026-10-25 | Local llama.cpp; **virtual** classification trees (files are not moved). |
| [mondaychen/tidybot](https://github.com/mondaychen/tidybot) | 109 | TS | Apache-2.0 | 2026-10-19 | CLI for an AI agent: sort/rename with a **reviewable plan and full undo**. |
| [thebearwithabite/ai-file-organizer](https://github.com/thebearwithabite/ai-file-organizer) | 50 | Python | MIT | 2026-10-11 | Vision + audio analysis, interactive classification, adaptive learning. |
| [thomashirtz/renaiming](https://github.com/thomashirtz/renaiming) | 51 | Python | — | 2024-02-06 | Rename and reorganize. |
| [octrow/ollama-rename-files](https://github.com/octrow/ollama-rename-files) | 46 | Python | MIT | 2026-10-22 | Content-based rename with Ollama, backs up originals. |
| [pega2077/ai_file_manager](https://github.com/pega2077/ai_file_manager) | 43 | TS | — | 2026-03-04 | Auto tag, classify, RAG over documents, images, videos. |
| [TheSethRose/AI-File-Organizer-Agent](https://github.com/TheSethRose/AI-File-Organizer-Agent) | 38 | Python | — | 2025-04-04 | Agent proposes, then executes a plan (Gemini). |
| [blankenshipz/clerk](https://github.com/blankenshipz/clerk) | 31 | Rust | — | 2023-03-24 | LLM file organizer in Rust (old). |
| [qup1010/FilePilot](https://github.com/qup1010/FilePilot) | 28 | Python | MIT | 2026-10-11 | Safe AI organizing of cluttered folders. |
| [xSarumo/TidyFS](https://github.com/xSarumo/TidyFS) | 19 | Go | MIT | 2026-06-27 | Linux TUI; TF-IDF + local LLM classification. |
| [yousefebrahimi0/Offline-AI-File-Organizer](https://github.com/yousefebrahimi0/Offline-AI-File-Organizer) | 19 | Python | — | 2025-12-23 | Offline rename + organize. |
| [ChenAI-TGF/FolderFox](https://github.com/ChenAI-TGF/FolderFox) | 15 | Python | — | 2026-03-16 | Suggests a subfolder structure; preview, adjust, apply. |
| [atlomak/norbot](https://github.com/atlomak/norbot) | 12 | Go | — | 2025-01-16 | Terminal LLM organizer. |
| [marksverdhei/sorting-hat](https://github.com/marksverdhei/sorting-hat) | 17 | Shell | — | 2026-06-27 | AI file naming with any OpenAI-compatible LLM. |

### Inbox-folder services (closest workflow, but tiny)

| Project | ★ | Notes |
|---|---:|---|
| [xntj-ai/guiwei](https://github.com/xntj-ai/guiwei) | 26 | Watches an **inbox folder** → LLM reads the content → renames and files it, with an undoable ledger. Dry run by default. **Sensitive files (matched by keyword) are never sent to the model.** Windows/PowerShell, CLI only. |
| [bsnydr/organizer](https://github.com/bsnydr/organizer) | 0 | *Below the star bar, but conceptually closest (and has our name).* macOS: `_inbox/` → Ollama → folder + filename + **confidence**. ≥0.80 is auto-filed, lower goes to a `_review/` queue. Per-run undo script. **Dropped folders are moved as a unit.** |

## 3. Document management with AI (closest "suggest and confirm" UX)

| Project | ★ | Lang | License | Last push | Notes |
|---|---:|---|---|---|---|
| [paperless-ngx/paperless-ngx](https://github.com/paperless-ngx/paperless-ngx) | 47.1k | Python | GPL-3.0 | 2026-10-25 | **3.x ships native LLM suggestions** (title, tags, correspondent, type, dates; reportedly also storage path), plus an **"Apply AI Suggestions" workflow action** (3.1+). Its older matching for storage paths is also auto-classified. Documents only, with its own archive layout, not a general file server. |
| [clusterzx/paperless-ai](https://github.com/clusterzx/paperless-ai) | 6.0k | JS | MIT | 2026-10-16 | Auto title/tags/correspondent/type + RAG chat. The maintainer says it is **not maintained** (rewrite in progress). |
| [icereed/paperless-gpt](https://github.com/icereed/paperless-gpt) | 2.8k | Go | MIT | 2026-10-20 | **Tag-triggered modes**: `paperless-gpt` = manual review in a web UI, `paperless-gpt-auto` = apply automatically. LLM vision OCR. |
| [papra-hq/papra](https://github.com/papra-hq/papra) | 5.6k | TS (**SolidJS**) | AGPL-3.0 | 2026-10-24 | Minimal document archive: email and folder ingestion, rule-based tagging. AI and mobile are "maybe one day". Solid frontend, worth looking at. |
| [eikek/docspell](https://github.com/eikek/docspell) | 2.3k | Elm/Scala | AGPL-3.0 | 2026-10-22 | Classic ML metadata suggestions for scanned piles. |
| [admonstrator/zettelrobbe](https://github.com/admonstrator/zettelrobbe) | 259 | JS | MIT | 2026-10-22 | Paperless-AI fork with an OCR rescue queue. |
| [nyxtron/paperless-aissist](https://github.com/nyxtron/paperless-aissist) | 78 | Python | MIT | 2026-10-06 | LLM metadata + vision OCR with a web UI. |
| [arturict/tagvico-ai](https://github.com/arturict/tagvico-ai) | 35 | TS | MIT | 2026-10-25 | **"Review first" vs "Automatic"** filing modes; every AI write is a proposal that an owner approves. |
| [knows-cloud/paperless-iq](https://github.com/knows-cloud/paperless-iq), [web-dienste/AI-Paperless-Organizer](https://github.com/web-dienste/AI-Paperless-Organizer), [lucaperl/paperless-local-ai](https://github.com/lucaperl/paperless-local-ai), [renefichtmueller/PaperCortex](https://github.com/renefichtmueller/PaperCortex), [ju6ge/paperless-llm-workflows](https://github.com/ju6ge/paperless-llm-workflows) (Rust) | 13–31 | — | — | 2026 | More Paperless AI add-ons. The space is crowded, which shows demand for "AI files it, human confirms". |

## 4. Links and bookmarks with AI and mobile share targets

| Project | ★ | Lang | License | Last push | Notes |
|---|---:|---|---|---|---|
| [karakeep-app/karakeep](https://github.com/karakeep-app/karakeep) | 30.0k | TS | AGPL-3.0 | 2026-10-25 | (ex-Hoarder) Links/notes/images/PDFs, **AI tagging + summaries with Ollama**, full-page archiving (monolith), iOS + Android apps, browser extensions. Organised by tags and lists, not folders. **Shows the mobile-share + local-AI pattern works for self-hosters.** |
| [linkwarden/linkwarden](https://github.com/linkwarden/linkwarden) | 20.2k | TS | AGPL-3.0 | 2026-10-25 | Bookmarks with full preservation (snapshots), AI tagging, collections. |
| [guozi/koby](https://github.com/guozi/koby) | 16 | Vue | — | 2026-08-06 | Bookmarks with **folder classification** + AI tag suggestions. |
| [ayoub9360/stash-bookmark](https://github.com/ayoub9360/stash-bookmark) | 13 | TS | — | 2026-02-28 | Paste a link → summary, category, tags. |

## 5. Tagging, knowledge and "second brain" tools

| Project | ★ | Notes |
|---|---:|---|
| [AgriciDaniel/claude-obsidian](https://github.com/AgriciDaniel/claude-obsidian) | 15.8k | "Drop any source and Claude reads, links, and files it" into an Obsidian vault. The same drop-and-file idea, for notes and cloud LLMs. |
| [tagspaces/tagspaces](https://github.com/tagspaces/tagspaces) | 5.3k | Offline file manager by tags; Ollama auto-tagging and summaries (Pro/beta). Tags instead of moving files. |
| [deta/surf](https://github.com/deta/surf) | 3.6k | AI notebooks that organize files and webpages. |
| [Nexus-JPF/note-companion](https://github.com/Nexus-JPF/note-companion) | 873 | Obsidian plugin (formerly "File Organizer 2000"): an inbox where AI files notes into folders. **Conceptually very close, for Obsidian notes.** |

## 6. Rule-based organizers (no AI, but same "inbox → rules → move" loop)

| Project | ★ | Lang | Notes |
|---|---:|---|---|
| [tfeldmann/organize](https://github.com/tfeldmann/organize) | 3.2k | Python | YAML rules for file automation. |
| [hsr88/mouzi](https://github.com/hsr88/mouzi) | 1.1k | TS | Tray app that sorts Downloads. |
| [lab421/forel](https://github.com/lab421/forel) | 894 | Swift | Open source Hazel alternative for macOS. |
| [ricardodantas/hazelnut](https://github.com/ricardodantas/hazelnut) | 323 | Rust | Hazel-like watcher TUI. |

## 7. Building blocks and references for our stack

| Project | ★ | Why it matters |
|---|---:|---|
| [messense/dav-server-rs](https://github.com/messense/dav-server-rs) | 243 | Rust WebDAV server library; likely our WebDAV layer. |
| [miquels/webdav-handler-rs](https://github.com/miquels/webdav-handler-rs) | 102 | Upstream it was forked from. |
| [stalwartlabs/stalwart](https://github.com/stalwartlabs/stalwart) | 15.3k | Rust server that speaks WebDAV among many protocols; good reference for single-binary Rust servers. |
| [nextcloud/desktop](https://github.com/nextcloud/desktop) | 3.9k | Reference for a sync client with virtual files (Windows Cloud Files, macOS File Provider). |
| [rgon/ncrsDesktop](https://github.com/rgon/ncrsDesktop) | 12 | Rust Nextcloud client with VFS on Linux, which is relevant if we want on-demand files on Linux too. |
| AtalayaLabs/OxiCloud, sigoden/dufs | — | Rust file servers to study (see section 1). |

## 8. Commercial / closed products

| Product | Notes |
|---|---|
| **Filex AI** | Hosted; iOS/Android/web. **Share files into the app → AI analyses and sorts them into folders** with custom rules. Closest UX match. Not self-hosted. |
| **The Drive AI** | Hosted AI drive; mobile scanner; "AI decides where it belongs". |
| **Sparkle** (Every) | Mac app that AI-organizes Downloads/Desktop/Documents into its own folder system. Subscription. |
| **Hazel** | Mac, rule-based (since 2007). |
| **Seafile 13 / Nextcloud Assistant** | See section 1: AI metadata and tagging, not placement. |
| **Google Drive, Dropbox** | AI search and suggestions; users still build their own folders. |

---

## What this means for Organizer

**The gap is real.** The self-hosted file servers have not made "AI places files" their core, and the AI organizers are all single-machine desktop or CLI tools. Paperless-ngx is the most likely to move into this space, but only for documents.

**Positioning:** "Nextcloud/Seafile-class file server whose inbox files things for you, confirming with you, using your own local model." The open source alternatives to compare against are Filex AI / The Drive AI (UX) and OxiCloud / Sync-in (tech).

**Ideas worth borrowing:**
- **Confidence-gated auto-accept** (bsnydr/organizer): a fourth pending mode, "auto-accept when confidence ≥ X", which may suit users better than a time-based auto-accept.
- **Learning from approved and corrected decisions as prompt hints** (AI File Sorter, llama-fs). This is already in our plan; their experience suggests it is what makes results consistent.
- **"Consistent vs refined" mode and label/folder allow-lists** (AI File Sorter) to stop the model inventing too many new folders.
- **Sensitive-file guard** (guiwei): keyword or path rules that never send content to the model and place by name only.
- **Undo ledger** for every applied placement (guiwei, tidybot, AI File Sorter).
- **Tag- or folder-triggered modes** (paperless-gpt): e.g. `/Inbox/review` vs `/Inbox/auto` over WebDAV.
- **Page snapshots with monolith** (Karakeep) for the "bookmark + snapshot" link mode.
- **LiteLLM-style provider abstraction** (Seafile AI). An OpenAI-compatible client covers it for us.
- **Keep files plain on disk** (Sync-in), the same choice we made; they show it works with WebDAV and desktop sync.

**Risks and notes:**
- **Name:** "Organizer" collides with [tfeldmann/organize](https://github.com/tfeldmann/organize), bsnydr/organizer and many others, so we will need a distinctive name before release.
- **Patent:** IBM's [US11748305B2](https://patents.google.com/patent/US11748305B2), "Suggesting a destination folder for a file to be saved", covers a related idea. I have not read its claims; worth checking with someone qualified before any commercial use. (Not legal advice.)
- **License landscape:** most of this space is AGPL-3.0 (Nextcloud, Karakeep, Linkwarden, Papra, AI File Sorter) or GPL-3.0 (Paperless-ngx). The Rust servers (OxiCloud, dufs) are MIT/Apache. This is relevant to the MIT vs Hippocratic vs GPL decision.

## Method

- GitHub search API (repositories, `stars:>10`, sorted by stars) for ~40 queries covering AI/LLM file organizing, renaming, inbox/drop folders, Ollama, document classification, self-hosted storage, WebDAV, sync, Rust file servers, Paperless AI, bookmark AI, Hazel alternatives.
- GitHub topic pages: `file-organizer`, `ai-file-organizer`.
- Web search for commercial products, Seafile 13 AI, Nextcloud AI, Paperless-ngx 3 AI, TagSpaces, Spacedrive, desktop sync clients with virtual files.
- READMEs read for the closest matches (llama-fs, AI File Sorter, OxiCloud, Karakeep, Paperless-AI, paperless-gpt, Tagvico, guiwei, bsnydr/organizer, Firefly, Sync-in, Papra, bewCloud). Paperless-ngx AI features confirmed from its release notes.
- Unrelated hits were excluded (AI coding workbenches, spam repos, generic LLM stacks).

### Sources (web)

- [Seafile AI extension (admin manual)](https://manual.seafile.com/latest/extension/seafile-ai/) · [Seafile 13.0 announcement](https://blog.seafile.com/seafile-13-0-is-ready-470c5e75f6fc) · [Metadata server](https://manual.seafile.com/latest/extension/metadata-server/)
- [Paperless-ngx 3 AI setup (wz-it)](https://wz-it.com/en/blog/paperless-ngx-ai-setup/) · [Paperless-ngx 3 local AI with Ollama (jacar.es)](https://jacar.es/en/paperless-ngx-3-local-ai-ollama/)
- [Nextcloud forum: external LLM for file organization](https://help.nextcloud.com/t/external-llm-integration-for-bokkmarks-and-files-organization/216924) · [Nextcloud Files automated tagging](https://apps.nextcloud.com/apps/files_automatedtagging)
- [TagSpaces AI](https://www.tagspaces.org/ai/) · [TagSpaces 6.3](https://www.tagspaces.org/blog/tagspaces-6-3/)
- [Filex AI](https://www.goodfirms.co/software/filex-ai) · [The Drive AI mobile](https://thedrive.ai/mobile) · [Best AI file organizers 2026 (fast.io)](https://www.fast.io/resources/best-ai-file-organizers-2026.md)
- [OxiCloud overview (pinggy)](https://pinggy.io/blog/oxicloud_self_hosted_cloud_storage/) · [Spacedrive mirror / v2 status](https://github.com/patevs/spacedrive)
- [GitHub topic: file-organizer](https://github.com/topics/file-organizer) · [GitHub topic: ai-file-organizer](https://github.com/topics/ai-file-organizer)
- [n8n: organise local directories with AI](https://n8n.io/workflows/2334-organise-your-local-file-directories-with-ai/)
- [US11748305B2 (Google Patents)](https://patents.google.com/patent/US11748305B2)
