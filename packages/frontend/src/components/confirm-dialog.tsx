import { useState } from "react";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";

/**
 * Asks before something is done that matters: delete, empty, replace. With
 * `confirmText`, asked twice: the button works once that text is typed
 * (removing a user: their email).
 */
export function ConfirmDialog({
  title,
  description,
  action,
  destructive = true,
  confirmText,
  onConfirm,
  onClose,
}: {
  title: string;
  description: React.ReactNode;
  /** The button that does it: "Delete", "Empty trash". */
  action: string;
  destructive?: boolean;
  /** What must be typed before the button works. */
  confirmText?: string;
  onConfirm: () => void;
  onClose: () => void;
}) {
  const [typed, setTyped] = useState("");
  const confirmed = confirmText === undefined || typed.trim() === confirmText;
  return (
    <AlertDialog open onOpenChange={(open) => !open && onClose()}>
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{title}</AlertDialogTitle>
          <AlertDialogDescription>{description}</AlertDialogDescription>
        </AlertDialogHeader>
        {confirmText !== undefined && (
          <Field>
            <FieldLabel htmlFor="confirm-text">
              Type <span className="font-mono">{confirmText}</span> to confirm
            </FieldLabel>
            <Input
              id="confirm-text"
              autoComplete="off"
              value={typed}
              onChange={(e) => setTyped(e.target.value)}
            />
          </Field>
        )}
        <AlertDialogFooter>
          <AlertDialogCancel>Cancel</AlertDialogCancel>
          <AlertDialogAction
            variant={destructive ? "destructive" : "default"}
            disabled={!confirmed}
            onClick={() => {
              if (!confirmed) return;
              onConfirm();
              onClose();
            }}
          >
            {action}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
