/** A figure with its label, for a row of status figures. */
export function Stat({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="rounded-lg bg-muted p-3">
      <p className="text-xs text-muted-foreground">{label}</p>
      <div className="mt-1 flex items-center gap-2 text-sm font-medium">{children}</div>
    </div>
  );
}
