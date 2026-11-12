import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { FieldError, FieldGroup } from "@/components/ui/field";
import { errorMessage } from "@/lib/api";

/**
 * A dialog that is a form: its title, its fields, what went wrong, and
 * Cancel beside the action. Rename, move, the desktop app's folders.
 */
export function FormDialog({
  title,
  description,
  action,
  pending,
  error,
  onSubmit,
  onClose,
  extra,
  children,
}: {
  title: string;
  description?: React.ReactNode;
  /** The submit button: "Rename", "Move". */
  action: string;
  pending: boolean;
  /** Shown under the fields when set: an error, or its message. */
  error?: unknown;
  onSubmit: () => void;
  onClose: () => void;
  /** More buttons, before Cancel: "Test". */
  extra?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent>
        {/* A form taller than the screen scrolls, over the dialog's padding (-m-4 p-4). */}
        <form
          className="-m-4 flex max-h-[calc(100dvh-2rem)] min-w-0 flex-col gap-4 overflow-y-auto p-4"
          onSubmit={(e) => {
            e.preventDefault();
            onSubmit();
          }}
        >
          <DialogHeader>
            <DialogTitle>{title}</DialogTitle>
            {description && <DialogDescription>{description}</DialogDescription>}
          </DialogHeader>
          <FieldGroup>{children}</FieldGroup>
          {error ? <FieldError>{errorMessage(error)}</FieldError> : null}
          <DialogFooter>
            {extra}
            <Button type="button" variant="outline" onClick={onClose}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending}>
              {action}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
