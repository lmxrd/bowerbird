/** A page of the window: its title, an action beside it, and its content. */
export function Page({
  title,
  action,
  children,
}: {
  title: React.ReactNode;
  action?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="flex flex-1 flex-col gap-4 p-6">
      <div className="flex items-center justify-between gap-4">
        <h1 className="text-lg font-medium">{title}</h1>
        {action}
      </div>
      {children}
    </div>
  );
}
