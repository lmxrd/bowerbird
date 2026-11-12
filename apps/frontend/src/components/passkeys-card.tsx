import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { KeyIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import { finishPasskeyRegistration, startPasskeyRegistration } from "organizer-sdk";
import type { PasskeyItem } from "organizer-sdk";
import { deletePasskeyMutation, listPasskeysOptions } from "organizer-sdk/react-query";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { ListRow } from "@/components/list-row";
import { SettingsCard } from "@/components/settings";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage, queryClient } from "@/lib/api";
import { formatDate } from "@/lib/format";
import { cancelled, createPasskey, passkeysSupported } from "@/lib/passkey";

/** The signed-in user's passkeys: add, list, remove. */
export function PasskeysCard({ available }: { available: boolean }) {
  const passkeys = useQuery({ ...listPasskeysOptions(), enabled: available });
  const [adding, setAdding] = useState(false);
  const [removing, setRemoving] = useState<PasskeyItem>();
  const remove = useMutation({
    ...deletePasskeyMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Removed. Remove it from your device's passwords too.");
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const usable = available && passkeysSupported();
  return (
    <SettingsCard
      title="Passkeys"
      description="Sign in with your fingerprint, face or device PIN, without a password or code. Your password stays as a fallback."
      error={passkeys.error}
    >
      <div className="flex flex-col gap-2">
        {!available && (
          <p className="text-sm text-muted-foreground">
            Passkeys need the server's address (PUBLIC_URL) set by the superadmin.
          </p>
        )}
        {available && !passkeysSupported() && (
          <p className="text-sm text-muted-foreground">This browser can't use passkeys.</p>
        )}
        <div className="flex flex-col divide-y">
          {passkeys.data?.map((passkey) => (
            <ListRow
              key={passkey.id}
              icon={<KeyIcon className="size-4 shrink-0 text-muted-foreground" />}
              title={passkey.name}
              detail={`Added ${formatDate(passkey.created_at)} · ${passkey.last_used_at ? `last used ${formatDate(passkey.last_used_at)}` : "never used"}`}
            >
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={`Remove ${passkey.name}`}
                onClick={() => setRemoving(passkey)}
              >
                <Trash2Icon />
              </Button>
            </ListRow>
          ))}
        </div>
        {usable && (
          <Button variant="outline" className="self-start" onClick={() => setAdding(true)}>
            Add a passkey
          </Button>
        )}
      </div>
      {adding && <AddPasskeyDialog onClose={() => setAdding(false)} />}
      {removing && (
        <ConfirmDialog
          title={`Remove ${removing.name}?`}
          description="It won't sign you in any more."
          action="Remove"
          onConfirm={() => remove.mutate({ path: { id: removing.id } })}
          onClose={() => setRemoving(undefined)}
        />
      )}
    </SettingsCard>
  );
}

/** A name, then the browser's own passkey prompt. */
function AddPasskeyDialog({ onClose }: { onClose: () => void }) {
  const [name, setName] = useState(guessDeviceName());
  const add = useMutation({
    mutationFn: async () => {
      const { data: started } = await startPasskeyRegistration({ throwOnError: true });
      const credential = await createPasskey(started.options);
      await finishPasskeyRegistration({
        body: { challenge_id: started.challenge_id, name, credential: credential },
        throwOnError: true,
      });
    },
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success("Passkey added");
      onClose();
    },
  });
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        <form
          className="flex flex-col gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            add.mutate();
          }}
        >
          <DialogHeader>
            <DialogTitle>Add a passkey</DialogTitle>
          </DialogHeader>
          <Field>
            <FieldLabel htmlFor="passkey-name">Name</FieldLabel>
            <Input
              id="passkey-name"
              value={name}
              maxLength={64}
              onChange={(e) => setName(e.target.value)}
              autoFocus
              required
            />
            {add.isError && !cancelled(add.error) && (
              <FieldError>{errorMessage(add.error)}</FieldError>
            )}
          </Field>
          <DialogFooter>
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={add.isPending}>
              Continue
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

/** A starting name for a passkey: the kind of device it is made on. */
function guessDeviceName(): string {
  const agent = navigator.userAgent;
  if (/iPhone/.test(agent)) return "iPhone";
  if (/iPad/.test(agent)) return "iPad";
  if (/Android/.test(agent)) return "Android phone";
  if (/Mac OS X/.test(agent)) return "Mac";
  if (/Windows/.test(agent)) return "Windows PC";
  return "Passkey";
}
