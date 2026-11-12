import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { DownloadIcon, LockIcon } from "lucide-react";
import { getEncryptionOptions } from "organizer-sdk/react-query";
import { CopyButton } from "@/components/copy-button";
import {
  ChangePassphraseDialog,
  RecoveryKeyDialog,
  UnlockDialog,
} from "@/components/encryption-dialogs";
import { LevelSettingsCard, useMySettings } from "@/components/levels";
import { ListRow } from "@/components/list-row";
import { SuperadminGate } from "@/components/step-up";
import { jsonOf, sourceName } from "@/lib/levels";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import { Field, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Switch } from "@/components/ui/switch";
import { queryClient } from "@/lib/api";
import { lock, passphraseProblem, setUp, useUnlocked } from "@/lib/encryption";

export const Route = createFileRoute("/_app/settings/encryption")({ component: Encryption });

const ABOUT =
  "Encrypted folders can be read only by you and the people you share them with: not by the server, its superadmins or its backups. No AI, search or links in them.";

function Encryption() {
  const { user } = Route.useRouteContext();
  const keys = useQuery(getEncryptionOptions());
  const [recovery, setRecovery] = useState<string>();
  // Turned off by the superadmin or a team: no new encrypted folders.
  const allowed = useMySettings().get("encryption.allowed");
  const offBy =
    allowed && jsonOf(allowed.value) === false
      ? sourceName(allowed.locked_by ?? allowed.source)
      : undefined;
  // The recovery key is shown once, right after setting up.
  if (recovery) return <RecoveryKey text={recovery} onDone={() => setRecovery(undefined)} />;
  return (
    <>
      <SettingsCard
        title="Encryption"
        description={ABOUT}
        enabled={keys.data ? keys.data.set_up : undefined}
        error={keys.error}
      >
        {offBy && (
          <p className="mb-4 text-sm text-muted-foreground">
            <LockIcon className="inline size-3" /> New encrypted folders are turned off: set by{" "}
            {offBy}. Those you have stay readable.
          </p>
        )}
        {keys.data &&
          (keys.data.set_up ? <SetUp /> : <NotSetUp onSetUp={(text) => setRecovery(text)} />)}
      </SettingsCard>
      {user.is_superadmin && (
        <SuperadminGate lazy title="For everyone">
          <LevelSettingsCard
            scope={{ level: "superadmin" }}
            prefixes={["encryption."]}
            title="For everyone"
            description="Whether users may make encrypted folders; a team may decide for its members when it is not locked."
          />
        </SuperadminGate>
      )}
    </>
  );
}

/** Not set up: the passphrase twice, then "Set up encryption". */
function NotSetUp({ onSetUp }: { onSetUp: (recovery: string) => void }) {
  const [passphrase, setPassphrase] = useState("");
  const [again, setAgain] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string>();
  const submit = async () => {
    const problem = passphraseProblem(passphrase, again);
    if (problem) return setError(problem);
    setPending(true);
    setError(undefined);
    try {
      const text = await setUp(passphrase);
      onSetUp(text);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Something went wrong");
    } finally {
      setPending(false);
    }
  };
  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <Field>
        <FieldLabel htmlFor="passphrase">Encryption passphrase</FieldLabel>
        <Input
          id="passphrase"
          type="password"
          autoComplete="new-password"
          value={passphrase}
          onChange={(e) => setPassphrase(e.target.value)}
          required
        />
      </Field>
      <Field>
        <FieldLabel htmlFor="again">Passphrase again</FieldLabel>
        <Input
          id="again"
          type="password"
          autoComplete="new-password"
          value={again}
          onChange={(e) => setAgain(e.target.value)}
          required
        />
      </Field>
      <p className="text-sm text-muted-foreground">
        Not your sign-in password: you type it on each device to unlock your encrypted folders.
      </p>
      {error && <FieldError>{error}</FieldError>}
      <Button type="submit" className="self-start" disabled={pending}>
        {pending ? "Setting up…" : "Set up encryption"}
      </Button>
    </form>
  );
}

/** The recovery key, shown once: copy or download it, say it is saved, Done. */
function RecoveryKey({ text, onDone }: { text: string; onDone: () => void }) {
  const [saved, setSaved] = useState(false);
  const download = () => {
    const blob = new Blob(
      [
        `Organizer encryption recovery key\n\n${text}\n\nIt opens your encrypted folders if you forget your passphrase. If you lose both, nobody can read them.\n`,
      ],
      { type: "text/plain" },
    );
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "organizer-recovery-key.txt";
    a.click();
    URL.revokeObjectURL(a.href);
  };
  return (
    <SettingsCard
      title="Your recovery key"
      description="If you forget your passphrase, this key is the only way back into your encrypted folders. Save it somewhere safe: it isn't shown again. If you lose both, nobody can read them."
    >
      <div className="flex flex-col gap-4">
        <code className="rounded-md bg-muted px-3 py-2 font-mono text-sm break-all">{text}</code>
        <div className="flex gap-2">
          <CopyButton text={text} copied="Recovery key copied" />
          <Button variant="outline" onClick={download}>
            <DownloadIcon />
            Download
          </Button>
        </div>
        <Field orientation="horizontal">
          <Switch id="saved" checked={saved} onCheckedChange={setSaved} />
          <FieldLabel htmlFor="saved">I have saved it</FieldLabel>
        </Field>
        <Button
          className="self-start"
          disabled={!saved}
          onClick={() => {
            void queryClient.invalidateQueries();
            onDone();
          }}
        >
          Done
        </Button>
      </div>
    </SettingsCard>
  );
}

/** Set up: this device's state, the passphrase, the recovery key. */
function SetUp() {
  const unlocked = useUnlocked();
  const [dialog, setDialog] = useState<"unlock" | "change" | "recovery">();
  return (
    <div className="flex flex-col divide-y">
      <ListRow
        title={unlocked ? "Unlocked on this device" : "Locked on this device"}
        detail={
          unlocked
            ? "Until you lock it or reload the page."
            : "Enter your passphrase to open your encrypted folders here."
        }
      >
        {unlocked ? (
          <Button variant="outline" size="sm" onClick={lock}>
            Lock
          </Button>
        ) : (
          <Button variant="outline" size="sm" onClick={() => setDialog("unlock")}>
            Unlock…
          </Button>
        )}
      </ListRow>
      <ListRow title="Change passphrase">
        <Button variant="outline" size="sm" onClick={() => setDialog("change")}>
          Change…
        </Button>
      </ListRow>
      <ListRow title="Forgot your passphrase?">
        <Button variant="outline" size="sm" onClick={() => setDialog("recovery")}>
          Use the recovery key…
        </Button>
      </ListRow>
      {dialog === "unlock" && <UnlockDialog onClose={() => setDialog(undefined)} />}
      {dialog === "change" && <ChangePassphraseDialog onClose={() => setDialog(undefined)} />}
      {dialog === "recovery" && <RecoveryKeyDialog onClose={() => setDialog(undefined)} />}
    </div>
  );
}
