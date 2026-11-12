import { useState } from "react";
import { toast } from "sonner";
import { FormDialog } from "@/components/form-dialog";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { changeToPassphrase, passphraseProblem, recoverWith, unlock } from "@/lib/encryption";

/** A password field of these dialogs. */
function Secret({
  id,
  label,
  value,
  onChange,
  autoFocus,
  description,
}: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
  autoFocus?: boolean;
  description?: string;
}) {
  return (
    <Field>
      <FieldLabel htmlFor={id}>{label}</FieldLabel>
      <Input
        id={id}
        type="password"
        autoComplete="off"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        autoFocus={autoFocus}
        required
      />
      {description && <FieldDescription>{description}</FieldDescription>}
    </Field>
  );
}

/** Runs `work`, with its error shown and the dialog kept until it is done. */
function useRun(onDone: () => void) {
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<unknown>();
  const run = async (work: () => Promise<void>) => {
    setPending(true);
    setError(undefined);
    try {
      await work();
      onDone();
    } catch (e) {
      setError(e);
    } finally {
      setPending(false);
    }
  };
  return { pending, error, setError, run };
}

/** Unlock on this device: the encryption passphrase. */
export function UnlockDialog({ onClose }: { onClose: () => void }) {
  const [passphrase, setPassphrase] = useState("");
  const { pending, error, run } = useRun(onClose);
  return (
    <FormDialog
      title="Unlock encrypted folders"
      description="Enter your encryption passphrase on this device."
      action={pending ? "Unlocking…" : "Unlock"}
      pending={pending}
      error={error}
      onSubmit={() => void run(() => unlock(passphrase))}
      onClose={onClose}
    >
      <Secret
        id="passphrase"
        label="Encryption passphrase"
        value={passphrase}
        onChange={setPassphrase}
        autoFocus
        description="Not your sign-in password. This browser keeps the key until you lock it or reload the page."
      />
    </FormDialog>
  );
}

/** Change the passphrase: the current one, then the new one twice. */
export function ChangePassphraseDialog({ onClose }: { onClose: () => void }) {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const { pending, error, setError, run } = useRun(() => {
    toast.success("Your encryption passphrase is changed");
    onClose();
  });
  return (
    <FormDialog
      title="Change passphrase"
      action="Change passphrase"
      pending={pending}
      error={error}
      onSubmit={() => {
        const problem = passphraseProblem(next, again);
        if (problem) return setError(problem);
        void run(() => changeToPassphrase(current, next));
      }}
      onClose={onClose}
    >
      <Secret
        id="current"
        label="Current passphrase"
        value={current}
        onChange={setCurrent}
        autoFocus
      />
      <Secret id="next" label="New passphrase" value={next} onChange={setNext} />
      <Secret id="again" label="New passphrase again" value={again} onChange={setAgain} />
    </FormDialog>
  );
}

/** A forgotten passphrase: the recovery key, then a new passphrase twice. */
export function RecoveryKeyDialog({ onClose }: { onClose: () => void }) {
  const [recovery, setRecovery] = useState("");
  const [next, setNext] = useState("");
  const [again, setAgain] = useState("");
  const { pending, error, setError, run } = useRun(() => {
    toast.success("Your encryption passphrase is changed");
    onClose();
  });
  return (
    <FormDialog
      title="Use the recovery key"
      description="The recovery key you saved when you set up encryption opens your keys; choose a new passphrase with it."
      action="Set new passphrase"
      pending={pending}
      error={error}
      onSubmit={() => {
        const problem = passphraseProblem(next, again);
        if (problem) return setError(problem);
        void run(() => recoverWith(recovery, next));
      }}
      onClose={onClose}
    >
      <Field>
        <FieldLabel htmlFor="recovery">Recovery key</FieldLabel>
        <Input
          id="recovery"
          className="font-mono"
          autoComplete="off"
          placeholder="XXXX-XXXX-…"
          value={recovery}
          onChange={(e) => setRecovery(e.target.value)}
          autoFocus
          required
        />
      </Field>
      <Secret id="next" label="New passphrase" value={next} onChange={setNext} />
      <Secret id="again" label="New passphrase again" value={again} onChange={setAgain} />
    </FormDialog>
  );
}
