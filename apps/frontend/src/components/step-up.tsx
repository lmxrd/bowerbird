import { useEffect, useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { KeyIcon, ShieldIcon } from "lucide-react";
import { startStepUpPasskey, stepUpPasskey, stepUpTotp } from "organizer-sdk";
import { client } from "organizer-sdk/client";
import { getSessionOptions, getSessionQueryKey } from "organizer-sdk/react-query";
import { EmptyState } from "@/components/empty-state";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldError, FieldGroup, FieldLabel, FieldSeparator } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage, queryClient } from "@/lib/api";
import { cancelled, getPasskey } from "@/lib/passkey";
import {
  isSuperadminUrl,
  oneAtATime,
  type Refusal,
  refusalOf,
  stepUpNeeded,
  totpCode,
} from "@/lib/step-up";

/**
 * The step-up of the superadmin's pages (docs/M7A_DESIGN.md §4; the dialog
 * approved 2026-11-09): "Confirm it's you" with a passkey or a two-factor
 * code, or, for a superadmin with neither, where to add one. Asked before
 * a superadmin page (`SuperadminGate`) and whenever one of their requests
 * is refused for it (`installStepUp`): the request is then sent again.
 */

/** Opens the dialog, while `StepUpProvider` is mounted. */
let opener: ((refusal: Refusal) => Promise<boolean>) | null = null;
/** Questions asked before it was: a page's effects run before its parents'. */
const waiting: (() => void)[] = [];

function setOpener(open: typeof opener) {
  opener = open;
  if (open) for (const ready of waiting.splice(0)) ready();
}

/** Ask for a step-up: true once confirmed, false when cancelled. */
const askStepUp = oneAtATime(async (refusal: Refusal): Promise<boolean> => {
  while (!opener) await new Promise<void>((ready) => waiting.push(ready));
  return opener(refusal);
});

/** The copies of the superadmin's requests, to send one again after a step-up. */
const copies = new WeakMap<Request, Request>();

/**
 * Once, at start: a superadmin request refused for a step-up opens the
 * dialog, and is sent again once confirmed (its answer is then the one the
 * caller gets); cancelled, the refusal goes through as it is.
 */
export function installStepUp() {
  client.interceptors.request.use((request) => {
    // The body is read when it is sent: a copy is kept for a second try.
    if (isSuperadminUrl(request.url)) copies.set(request, request.clone());
    return request;
  });
  client.interceptors.response.use(async (response, request, opts) => {
    const copy = copies.get(request);
    copies.delete(request);
    if (response.status !== 403 || !copy) return response;
    const refusal = refusalOf(
      await response
        .clone()
        .json()
        .catch(() => null),
    );
    if (!refusal || !(await askStepUp(refusal))) return response;
    return (opts.fetch ?? fetch)(copy);
  });
}

/** The dialog, for the signed-in app: mounted once, around its pages. */
export function StepUpProvider({ children }: { children: React.ReactNode }) {
  const [question, setQuestion] = useState<{
    refusal: Refusal;
    answer: (confirmed: boolean) => void;
  } | null>(null);
  useEffect(() => {
    setOpener((refusal) => new Promise((answer) => setQuestion({ refusal, answer })));
    return () => setOpener(null);
  }, []);
  return (
    <>
      {children}
      {question && (
        <StepUpDialog
          refusal={question.refusal}
          onDone={(confirmed) => {
            question.answer(confirmed);
            setQuestion(null);
          }}
        />
      )}
    </>
  );
}

function StepUpDialog({
  refusal,
  onDone,
}: {
  refusal: Refusal;
  onDone: (confirmed: boolean) => void;
}) {
  const session = useQuery(getSessionOptions());
  const methods = session.data?.step_up;
  const passkey = !!methods?.passkey;
  const totp = !!methods?.totp;
  const [code, setCode] = useState("");
  const confirmed = () => {
    void queryClient.invalidateQueries({ queryKey: getSessionQueryKey() });
    onDone(true);
  };
  const withCode = useMutation({
    mutationFn: (code: string) => stepUpTotp({ body: { code }, throwOnError: true }),
    onSuccess: confirmed,
  });
  const withPasskey = useMutation({
    mutationFn: async () => {
      const { data: started } = await startStepUpPasskey({ throwOnError: true });
      const credential = await getPasskey(started.options);
      await stepUpPasskey({
        body: { challenge_id: started.challenge_id, credential },
        throwOnError: true,
      });
    },
    onSuccess: confirmed,
  });
  const close = (open: boolean) => !open && onDone(false);

  if (refusal === "second_factor_required" || (session.data && !passkey && !totp)) {
    return (
      <Dialog open onOpenChange={close}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Add a passkey or two-factor first</DialogTitle>
            <DialogDescription>
              Settings of the whole server ask you to confirm it's you with a passkey or a code from
              an authenticator app, and you have neither yet. Add one in Settings → Security.
            </DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="outline" onClick={() => onDone(false)}>
              Cancel
            </Button>
            <Button render={<Link to="/settings/security" />} onClick={() => onDone(false)}>
              Go to Security
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    );
  }

  const valid = totpCode(code);
  const error = withCode.error ?? (cancelled(withPasskey.error) ? null : withPasskey.error);
  return (
    <Dialog open onOpenChange={close}>
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (totp && valid) withCode.mutate(valid);
            else if (passkey && !totp) withPasskey.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>Confirm it's you</DialogTitle>
            <DialogDescription>
              These settings change the whole server, so they ask for your passkey or a code from
              your authenticator app again. You won't be asked for 15 minutes.
            </DialogDescription>
          </DialogHeader>
          <FieldGroup>
            {passkey && (
              <Button
                type="button"
                variant="outline"
                disabled={withPasskey.isPending}
                onClick={() => withPasskey.mutate()}
              >
                <KeyIcon />
                Use passkey
              </Button>
            )}
            {passkey && totp && <FieldSeparator>or</FieldSeparator>}
            {totp && (
              <Field>
                <FieldLabel htmlFor="step-up-code">Code from your authenticator app</FieldLabel>
                <Input
                  id="step-up-code"
                  inputMode="numeric"
                  autoComplete="one-time-code"
                  placeholder="123456"
                  autoFocus
                  value={code}
                  onChange={(e) => setCode(e.target.value)}
                />
              </Field>
            )}
            {error ? <FieldError>{errorMessage(error)}</FieldError> : null}
          </FieldGroup>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={() => onDone(false)}>
              Cancel
            </Button>
            <Button
              type="submit"
              disabled={withCode.isPending || withPasskey.isPending || (totp ? !valid : !passkey)}
            >
              Confirm
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/**
 * A superadmin page, or part of one: asked for a step-up first when the
 * session has none (a dialog, then the page). `lazy` waits for a click
 * instead, for a part of a page everyone sees (the retention defaults).
 */
export function SuperadminGate({
  title = "Confirm it's you",
  lazy = false,
  children,
}: {
  /** What the placeholder says, before or after a cancelled dialog. */
  title?: string;
  lazy?: boolean;
  children: React.ReactNode;
}) {
  const session = useQuery(getSessionOptions());
  const [needed] = useState(() => stepUpNeeded(session.data?.step_up));
  const [state, setState] = useState<"asking" | "waiting" | "open">(
    needed === null ? "open" : lazy ? "waiting" : "asking",
  );
  useEffect(() => {
    if (state !== "asking" || needed === null) return;
    let live = true;
    void askStepUp(needed).then((ok) => {
      if (live) setState(ok ? "open" : "waiting");
    });
    return () => {
      live = false;
    };
  }, [state, needed]);
  if (state === "open") return children;
  if (state === "asking") return null;
  return (
    <EmptyState
      icon={ShieldIcon}
      title={title}
      action={<Button onClick={() => setState("asking")}>Confirm it's you</Button>}
    >
      {needed === "second_factor_required"
        ? "Settings of the whole server need a passkey or two-factor authentication: add one in Settings → Security."
        : "Settings of the whole server ask for your passkey or a two-factor code first."}
    </EmptyState>
  );
}
