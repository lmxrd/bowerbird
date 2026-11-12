import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { Loader2Icon } from "lucide-react";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Field, FieldDescription, FieldError, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { errorMessage } from "@/lib/api";
import { commands } from "../lib/desktop";

/**
 * Sign in: the server's address, then the system browser (the server's own
 * sign-in, with single sign-on, two-factor or a passkey as it has them),
 * which comes back to the app with the token.
 */
export function SignIn() {
  const [server, setServer] = useState("");
  const signIn = useMutation({ mutationFn: commands.signIn });
  return (
    <div className="flex min-h-svh items-center justify-center p-6">
      <Card className="w-full max-w-sm">
        <form
          className="flex flex-col gap-6"
          onSubmit={(e) => {
            e.preventDefault();
            signIn.mutate(server);
          }}
        >
          <CardHeader>
            <CardTitle>Sign in to Organizer</CardTitle>
            <CardDescription>
              Keep folders of your server in sync with this computer.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <Field>
              <FieldLabel htmlFor="server">Server address</FieldLabel>
              <Input
                id="server"
                placeholder="files.example.com"
                autoFocus
                required
                value={server}
                disabled={signIn.isPending}
                onChange={(e) => setServer(e.target.value)}
              />
              {signIn.isPending ? (
                <FieldDescription>Finish signing in in your browser…</FieldDescription>
              ) : (
                <FieldDescription>You sign in in your browser.</FieldDescription>
              )}
              {signIn.isError && <FieldError>{errorMessage(signIn.error)}</FieldError>}
            </Field>
          </CardContent>
          <CardFooter>
            <Button type="submit" className="w-full" disabled={signIn.isPending}>
              {signIn.isPending && <Loader2Icon className="animate-spin" />}
              Sign in with the browser
            </Button>
          </CardFooter>
        </form>
      </Card>
    </div>
  );
}
