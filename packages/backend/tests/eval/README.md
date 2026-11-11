# The drop box's eval set

Synthetic, made from templates (no real person's data): `tree.txt` is a
household's folders (one per line, `path | a file, another` for the files
shown as examples), `samples.jsonl` 200 items, each with its `name`, maybe
a `note` and the `text` read from it, and the `expected` folder.

Run against the model of the `AI_*` variables (manual, never in CI):

    AI_PROVIDER=openai AI_ENDPOINT=http://localhost:11434/v1 AI_MODEL=qwen3 mise run eval

`-- --freedom balanced` runs one folder-freedom setting (default: all
three), `-- --limit 20` the first 20 samples. A proposal is right when it
names the expected folder, close when it names one of its parents or a new
folder under it.
