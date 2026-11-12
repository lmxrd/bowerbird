import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { Trash2Icon, XIcon } from "lucide-react";
import { toast } from "sonner";
import type {
  DropBoxSettings,
  DroppedFolders,
  FolderFreedom,
  Links,
  PendingMode,
  ReadContents,
  RuleAction,
  RuleKind,
} from "organizer-sdk/types";
import {
  createGuardRuleMutation,
  deleteGuardRuleMutation,
  getDropBoxSettingsOptions,
  listGuardRulesOptions,
  updateDropBoxSettingsMutation,
} from "organizer-sdk/react-query";
import { AiRecipient } from "@/components/ai-recipient";
import {
  LevelNote,
  LevelSettingsCard,
  MySettingField,
  lockedForMe,
  useMySettings,
} from "@/components/levels";
import { SettingsCard } from "@/components/settings";
import { SuperadminGate } from "@/components/step-up";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Field,
  FieldDescription,
  FieldError,
  FieldGroup,
  FieldLabel,
  FieldSet,
  FieldLegend,
} from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Item,
  ItemActions,
  ItemContent,
  ItemGroup,
  ItemSeparator,
  ItemTitle,
} from "@/components/ui/item";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { errorMessage, queryClient } from "@/lib/api";
import { Fragment } from "react";

export const Route = createFileRoute("/_app/settings/drop-box")({ component: DropBoxSettingsPage });

type Option<T> = { value: T; label: string };

const readContents: Option<ReadContents>[] = [
  { value: "always", label: "For every item" },
  { value: "per_item", label: "I choose per item" },
  { value: "default_on", label: "On, I can turn it off per item" },
];
const freedom: Option<FolderFreedom>[] = [
  { value: "balanced", label: "Balanced" },
  { value: "consistent", label: "Consistent: prefer existing folders" },
  { value: "refined", label: "Refined: more specific folders" },
];
const links: Option<Links>[] = [
  { value: "bookmark", label: "Bookmark" },
  { value: "snapshot", label: "Bookmark and page snapshot" },
  { value: "download", label: "Download the file" },
];
const droppedFolders: Option<DroppedFolders>[] = [
  { value: "whole", label: "Keep a folder whole" },
  { value: "split", label: "Split it into its files" },
];
const topFolders: Option<"any" | "only">[] = [
  { value: "any", label: "Any" },
  { value: "only", label: "Only the ones I choose" },
];
const ruleKinds: Option<RuleKind>[] = [
  { value: "name", label: "Name" },
  { value: "path", label: "Path" },
  { value: "keyword", label: "Keyword" },
];
const ruleActions: Option<RuleAction>[] = [
  { value: "name_only", label: "Name only" },
  { value: "by_hand", label: "Nothing sent, I place it" },
];
const label = <T,>(options: Option<T>[], value: T) =>
  options.find((o) => o.value === value)?.label ?? String(value);

/** A stock Select over a list of options. */
function Choose<T extends string>({
  id,
  options,
  value,
  onChange,
  disabled,
  className = "w-full",
}: {
  id?: string;
  options: Option<T>[];
  value: T;
  onChange: (value: T) => void;
  disabled?: boolean;
  className?: string;
}) {
  return (
    <Select
      items={options}
      value={value}
      disabled={disabled}
      onValueChange={(v) => v && onChange(v)}
    >
      <SelectTrigger id={id} className={className}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent>
        {options.map((o) => (
          <SelectItem key={o.value} value={o.value}>
            {o.label}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

function DropBoxSettingsPage() {
  const settings = useQuery(getDropBoxSettingsOptions());
  const [form, setForm] = useState<DropBoxSettings>();
  // Kept as typed, so that a field can be emptied while editing.
  const [hours, setHours] = useState("24");
  const [threshold, setThreshold] = useState("80");
  const [folder, setFolder] = useState("");
  const { user } = Route.useRouteContext();
  const levels = useMySettings();
  const models = [...levels.values()].filter((s) => s.key.startsWith("ai.model."));
  useEffect(() => {
    const data = settings.data;
    if (!data) return;
    const { recipient: _, ...rest } = data;
    setForm(rest);
    setHours(String(data.accept_after_hours ?? 24));
    setThreshold(String(Math.round((data.confidence_threshold ?? 0.8) * 100)));
  }, [settings.data]);
  const save = useMutation({
    ...updateDropBoxSettingsMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Saved");
    },
  });
  const set = (change: Partial<DropBoxSettings>) => form && setForm({ ...form, ...change });
  const allowed = form?.allowed_top_folders;
  const addFolder = () => {
    const name = folder.trim();
    if (!name || !form) return;
    if (!allowed?.includes(name)) set({ allowed_top_folders: [...(allowed ?? []), name] });
    setFolder("");
  };

  return (
    <>
      {settings.isError && (
        <p className="text-sm text-destructive">{errorMessage(settings.error)}</p>
      )}
      {form && (
        <form
          className="flex flex-col gap-6"
          onSubmit={(e) => {
            e.preventDefault();
            save.mutate({
              body: {
                ...form,
                accept_after_hours: Number(hours),
                confidence_threshold: Number(threshold) / 100,
              },
            });
          }}
        >
          <Card>
            <CardHeader>
              <CardTitle>When a place is proposed</CardTitle>
            </CardHeader>
            <CardContent>
              <RadioGroup
                value={form.pending_mode}
                disabled={lockedForMe(levels.get("drop_box.pending_mode"))}
                onValueChange={(value) => set({ pending_mode: value as PendingMode })}
              >
                <Label className="font-normal">
                  <RadioGroupItem value="move_pending" />
                  Move it there now, and mark it pending until I confirm
                </Label>
                <Label className="font-normal">
                  <RadioGroupItem value="stay" />
                  Keep it in the drop box until I confirm
                </Label>
                <Label className="flex-wrap font-normal">
                  <RadioGroupItem value="timeout" />
                  Move it now, and accept it after
                  <Input
                    type="number"
                    aria-label="Hours"
                    min={1}
                    max={720}
                    className="w-20"
                    value={hours}
                    disabled={lockedForMe(levels.get("drop_box.accept_after_hours"))}
                    onChange={(e) => setHours(e.target.value)}
                    onFocus={() => set({ pending_mode: "timeout" })}
                  />
                  hours
                </Label>
                <Label className="flex-wrap font-normal">
                  <RadioGroupItem value="confidence" />
                  Accept it at once when the AI is at least
                  <Input
                    type="number"
                    aria-label="Percent"
                    min={1}
                    max={100}
                    className="w-20"
                    value={threshold}
                    disabled={lockedForMe(levels.get("drop_box.confidence_threshold"))}
                    onChange={(e) => setThreshold(e.target.value)}
                    onFocus={() => set({ pending_mode: "confidence" })}
                  />
                  % sure, else ask me
                </Label>
              </RadioGroup>
              <LevelNote setting={levels.get("drop_box.pending_mode")} />
              <LevelNote setting={levels.get("drop_box.accept_after_hours")} />
              <LevelNote setting={levels.get("drop_box.confidence_threshold")} />
            </CardContent>
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>How it files</CardTitle>
            </CardHeader>
            <CardContent>
              <FieldGroup>
                <Field>
                  <FieldLabel htmlFor="read-contents">Read contents</FieldLabel>
                  <Choose
                    id="read-contents"
                    options={readContents}
                    value={form.read_contents}
                    disabled={lockedForMe(levels.get("drop_box.read_contents"))}
                    onChange={(read_contents) => set({ read_contents })}
                  />
                  <LevelNote setting={levels.get("drop_box.read_contents")} />
                </Field>
                <Field>
                  <FieldLabel htmlFor="freedom">New folders</FieldLabel>
                  <Choose
                    id="freedom"
                    options={freedom}
                    value={form.folder_freedom}
                    disabled={lockedForMe(levels.get("drop_box.folder_freedom"))}
                    onChange={(folder_freedom) => set({ folder_freedom })}
                  />
                  <LevelNote setting={levels.get("drop_box.folder_freedom")} />
                </Field>
                <Field>
                  <FieldLabel htmlFor="top">Top-level folders</FieldLabel>
                  <Choose
                    id="top"
                    options={topFolders}
                    value={allowed ? "only" : "any"}
                    onChange={(value) =>
                      set({ allowed_top_folders: value === "only" ? (allowed ?? []) : null })
                    }
                  />
                  {allowed && (
                    <>
                      {allowed.length > 0 && (
                        <div className="flex flex-wrap gap-1">
                          {allowed.map((name) => (
                            <Badge key={name} variant="secondary">
                              {name}
                              <button
                                type="button"
                                aria-label={`Remove ${name}`}
                                onClick={() =>
                                  set({ allowed_top_folders: allowed.filter((n) => n !== name) })
                                }
                              >
                                <XIcon />
                              </button>
                            </Badge>
                          ))}
                        </div>
                      )}
                      <div className="flex gap-2">
                        <Input
                          aria-label="Folder name"
                          placeholder="Documents"
                          value={folder}
                          onChange={(e) => setFolder(e.target.value)}
                          onKeyDown={(e) => {
                            if (e.key === "Enter") {
                              e.preventDefault();
                              addFolder();
                            }
                          }}
                        />
                        <Button type="button" variant="outline" onClick={addFolder}>
                          Add folder
                        </Button>
                      </div>
                      <FieldDescription>
                        The drop box places items only in these, at the top of your tree.
                      </FieldDescription>
                    </>
                  )}
                </Field>
                <Field>
                  <FieldLabel htmlFor="dropped-folders">Dropped folders</FieldLabel>
                  <Choose
                    id="dropped-folders"
                    options={droppedFolders}
                    value={form.folders}
                    onChange={(folders) => set({ folders })}
                  />
                </Field>
                <Field>
                  <FieldLabel htmlFor="links">Shared links</FieldLabel>
                  <Choose
                    id="links"
                    options={links}
                    value={form.links}
                    onChange={(value) => set({ links: value })}
                  />
                </Field>
              </FieldGroup>
            </CardContent>
          </Card>
          {save.isError && <FieldError>{errorMessage(save.error)}</FieldError>}
          <Button type="submit" className="self-start" disabled={save.isPending}>
            Save
          </Button>
        </form>
      )}

      <GuardRules />

      {models.length > 0 && (
        <SettingsCard
          title="AI models"
          description="The model that does each kind of work for you, of those your administrator set up."
        >
          <FieldGroup>
            {models.map((setting) => (
              <MySettingField key={setting.key} setting={setting} />
            ))}
          </FieldGroup>
        </SettingsCard>
      )}

      {user.is_superadmin && (
        <SuperadminGate lazy title="For everyone">
          <LevelSettingsCard
            scope={{ level: "superadmin" }}
            prefixes={["drop_box."]}
            title="For everyone"
            description="What every user's drop box does: a default they may change, or locked."
          />
        </SuperadminGate>
      )}

      <Card>
        <CardHeader>
          <CardTitle>Drop box drive</CardTitle>
          <CardDescription>
            Mount <code>{window.location.origin}/dav-inbox/</code> with an{" "}
            <Link to="/settings/devices" className="underline underline-offset-4">
              app password
            </Link>
            . Files in review always wait for you; in auto they are placed at once.
          </CardDescription>
        </CardHeader>
      </Card>

      {settings.data && <AiRecipient recipient={settings.data.recipient} />}
    </>
  );
}

/** The sensitive-file rules: what is kept from the AI. */
function GuardRules() {
  const rules = useQuery(listGuardRulesOptions());
  const [kind, setKind] = useState<RuleKind>("name");
  const [pattern, setPattern] = useState("");
  const [action, setAction] = useState<RuleAction>("name_only");
  const create = useMutation({
    ...createGuardRuleMutation(),
    onSuccess: () => {
      setPattern("");
      void queryClient.invalidateQueries();
    },
  });
  const remove = useMutation({
    ...deleteGuardRuleMutation(),
    onSuccess: () => void queryClient.invalidateQueries(),
    onError: (error) => toast.error(errorMessage(error)),
  });
  return (
    <Card>
      <CardHeader>
        <CardTitle>Sensitive files</CardTitle>
        <CardDescription>Items that match are kept from the AI.</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-col gap-4">
        {rules.isError && <p className="text-sm text-destructive">{errorMessage(rules.error)}</p>}
        {rules.data && rules.data.length > 0 && (
          <ItemGroup className="gap-0 rounded-lg border">
            {rules.data.map((rule, i) => (
              <Fragment key={rule.id}>
                {i > 0 && <ItemSeparator className="my-0" />}
                <Item size="sm">
                  <ItemContent className="min-w-0">
                    <ItemTitle>
                      <Badge variant="outline">{label(ruleKinds, rule.kind)}</Badge>
                      <code className="truncate">{rule.pattern}</code>
                    </ItemTitle>
                  </ItemContent>
                  <span className="text-sm text-muted-foreground">
                    {label(ruleActions, rule.action)}
                  </span>
                  <ItemActions>
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`Delete the rule ${rule.pattern}`}
                      onClick={() => remove.mutate({ path: { id: rule.id } })}
                    >
                      <Trash2Icon />
                    </Button>
                  </ItemActions>
                </Item>
              </Fragment>
            ))}
          </ItemGroup>
        )}
        <form
          onSubmit={(e) => {
            e.preventDefault();
            create.mutate({ body: { kind, pattern: pattern.trim(), action } });
          }}
        >
          <FieldSet>
            <FieldLegend variant="label">Add a rule</FieldLegend>
            <div className="flex flex-wrap gap-2">
              <Choose className="w-28" options={ruleKinds} value={kind} onChange={setKind} />
              <Input
                aria-label="Pattern"
                placeholder="*.kdbx"
                className="min-w-40 flex-1"
                value={pattern}
                required
                onChange={(e) => setPattern(e.target.value)}
              />
              <Choose className="w-56" options={ruleActions} value={action} onChange={setAction} />
              <Button type="submit" variant="outline" disabled={create.isPending}>
                Add rule
              </Button>
            </div>
            {create.isError && <FieldError>{errorMessage(create.error)}</FieldError>}
            <FieldDescription>
              Name and path patterns are globs, case ignored: <code>*</code> any characters (slashes
              included), <code>?</code> one character, <code>[abc]</code> one of these,{" "}
              <code>[a-z]</code> a range, <code>{"{pdf,docx}"}</code> either. Put a backslash before
              a character to match it literally (<code>\*</code> is a star, <code>\[</code> a
              bracket). A keyword is a whole word (or words) found in the name, the note or the
              text, case ignored.
            </FieldDescription>
          </FieldSet>
        </form>
      </CardContent>
    </Card>
  );
}
