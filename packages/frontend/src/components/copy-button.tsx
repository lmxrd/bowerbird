import { CopyIcon } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";

/** Copies a text (a token, a link) to the clipboard, and says so. */
export function CopyButton({
  text,
  label = "Copy",
  copied = "Copied",
  variant = "outline",
  size,
}: {
  text: string;
  label?: string;
  /** What the toast says. */
  copied?: string;
  variant?: React.ComponentProps<typeof Button>["variant"];
  size?: React.ComponentProps<typeof Button>["size"];
}) {
  return (
    <Button
      type="button"
      variant={variant}
      size={size}
      onClick={() => {
        navigator.clipboard.writeText(text).then(
          () => toast.success(copied),
          () => toast.error("Couldn't copy: select it and copy it by hand"),
        );
      }}
    >
      <CopyIcon />
      {label}
    </Button>
  );
}
