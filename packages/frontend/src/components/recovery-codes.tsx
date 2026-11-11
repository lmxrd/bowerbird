import { Button } from "@/components/ui/button";

/** Recovery codes, shown once, with a way to copy them. */
export function RecoveryCodes({ codes }: { codes: string[] }) {
  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-muted-foreground">
        Each code signs you in once if you lose your authenticator app. Keep them somewhere safe:
        they aren't shown again.
      </p>
      <ul className="grid grid-cols-2 gap-1 rounded-lg border p-3 font-mono text-sm">
        {codes.map((code) => (
          <li key={code}>{code}</li>
        ))}
      </ul>
      <Button
        variant="outline"
        className="self-start"
        onClick={() => navigator.clipboard.writeText(codes.join("\n"))}
      >
        Copy codes
      </Button>
    </div>
  );
}

/** The secret of a new authenticator app, to scan as a link or to type. */
export function TotpSecret({ secret, uri }: { secret: string; uri: string }) {
  return (
    <div className="flex flex-col gap-2 text-sm">
      <p className="text-muted-foreground">
        Add Organizer to your authenticator app: open the link on this device, or type the key.
      </p>
      <a className="underline underline-offset-4" href={uri}>
        Open in authenticator app
      </a>
      <code className="rounded-md bg-muted px-2 py-1 font-mono break-all">{secret}</code>
    </div>
  );
}
