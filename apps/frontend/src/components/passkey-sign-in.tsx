import { useMutation } from "@tanstack/react-query";
import { KeyIcon } from "lucide-react";
import { finishPasskeySignIn, startPasskeySignIn } from "organizer-sdk";
import { Button } from "@/components/ui/button";
import { FieldError } from "@/components/ui/field";
import { errorMessage } from "@/lib/api";
import { cancelled, getPasskey } from "@/lib/passkey";

/** "Sign in with a passkey": the browser's prompt, then signed in. */
export function PasskeySignIn({ onDone }: { onDone: () => void }) {
  const signIn = useMutation({
    mutationFn: async () => {
      const { data: started } = await startPasskeySignIn({ throwOnError: true });
      const credential = await getPasskey(started.options);
      await finishPasskeySignIn({
        body: { challenge_id: started.challenge_id, credential: credential },
        throwOnError: true,
      });
    },
    onSuccess: onDone,
  });
  return (
    <>
      <Button onClick={() => signIn.mutate()} disabled={signIn.isPending}>
        <KeyIcon />
        Sign in with a passkey
      </Button>
      {signIn.isError && !cancelled(signIn.error) && (
        <FieldError>{errorMessage(signIn.error)}</FieldError>
      )}
    </>
  );
}
