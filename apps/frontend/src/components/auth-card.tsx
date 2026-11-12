import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";

/** A page before the app: setup, login, sign-up, an invite. One card, centred. */
export function AuthCard({
  title,
  description,
  children,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <div className="flex min-h-svh items-center justify-center bg-muted p-6">
      <Card className="w-full max-w-sm">
        <CardHeader>
          <CardTitle>{title}</CardTitle>
          {description && <CardDescription>{description}</CardDescription>}
        </CardHeader>
        <CardContent className="flex flex-col gap-4">{children}</CardContent>
      </Card>
    </div>
  );
}

/** The "or" between two ways in. */
export function OrSeparator() {
  return <p className="text-center text-sm text-muted-foreground">or</p>;
}
