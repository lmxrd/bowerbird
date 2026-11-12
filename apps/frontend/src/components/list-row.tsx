/** A line of a list in a dialog: what, a detail under it, and its actions. */
export function ListRow({
  title,
  detail,
  icon,
  children,
}: {
  title: React.ReactNode;
  detail?: React.ReactNode;
  icon?: React.ReactNode;
  children?: React.ReactNode;
}) {
  return (
    <div className="flex items-center gap-2 py-2">
      {icon}
      {/* No width of its own (w-0): a long line is cut here rather than widen the page. */}
      <div className="w-0 min-w-0 flex-1">
        <div className="truncate text-sm">{title}</div>
        {detail && <div className="truncate text-xs text-muted-foreground">{detail}</div>}
      </div>
      {children}
    </div>
  );
}
