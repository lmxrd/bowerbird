import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Role } from "organizer-sdk/types";

/**
 * The app's side (src-tauri): what the window shows, as one value the app
 * sends again whenever it changes (the `state` event), and the commands.
 * The types are those of `src-tauri/src/lib.rs` and of organizer-sync's
 * status: change both together.
 */

export type Phase = "up_to_date" | "syncing" | "paused" | "offline" | "error";

export type ActivityKind =
  | "upload"
  | "download"
  | "move"
  | "delete"
  | "conflict"
  | "drop_box"
  | "restore";

export type Activity = { kind: ActivityKind; path: string; at: string };

export type Conflict = { path: string; copy: string; at: string };

/**
 * Why a local change is not sent (docs/M7B_DESIGN.md §7): in a folder you
 * may only view; what you added where you may only upload (changed,
 * renamed or moved); or no longer seen on the server (limited access, your
 * access removed); or waiting for its drive's storage (docs/M7C_DESIGN.md):
 * unavailable, or the drive being moved.
 */
export type Held = "view_only" | "upload_only" | "hidden" | "unavailable" | "moving";

/**
 * A place kept here as it is, not synced: from the synced folder's name;
 * `reason`, the server's words when it said some (why the storage is
 * unavailable, where the drive is being moved).
 */
export type NotSynced = { path: string; why: Held; reason?: string };

/** A synced folder waiting for its drive's storage: `unavailable` or `moving`, and why. */
export type Waiting = { folder: number; why: Held; reason: string };

/** The encrypted folders, as this computer sees them. */
export type Encryption = {
  /** The account has set up encryption. */
  set_up: boolean;
  /** Unlocked here: the encrypted folders are synced. */
  unlocked: boolean;
  /** Encrypted folders left alone: locked here, or without a key for you yet. */
  skipped: number;
};

export type Status = {
  phase: Phase;
  pending: number;
  done: number;
  folders: number;
  recent: Activity[];
  conflicts: Conflict[];
  error: string | null;
  synced_at: string | null;
  encryption: Encryption;
  /** The synced folders (ids) no longer on the server, or whose drive you left or was deleted. */
  unavailable: number[];
  /** What your role keeps from being sent, and what is no longer seen: kept here, not synced. */
  not_synced: NotSynced[];
  /** The synced folders whose drive's storage is unavailable, or which is being moved. */
  waiting: Waiting[];
};

/** How a folder is kept: in full, or its files downloaded when opened (macOS and Windows). */
export type FolderMode = "sync" | "on_demand";

export type SyncedFolder = {
  id: number;
  name: string;
  local_path: string;
  mode: FolderMode;
  /** The folder of the server synced: a drive's root, or a folder in a drive. */
  remote_id: string;
  /** The drive it is in; null until the engine has asked the server (before drives: My drive). */
  drive_id: string | null;
  /** No longer on the server, or its drive left or deleted: not synced, its files kept here. */
  unavailable: boolean;
  /** Its drive's storage is unavailable, or the drive is being moved: not synced until then. */
  waiting: Waiting | null;
  /** What you may do with it, as last listed; null before. */
  role: Role | null;
};

/** How this system keeps on-demand folders; null: full sync only (Linux). */
export type OnDemand = "file_provider" | "cloud_files" | null;

export type View = {
  signed_in: boolean;
  server: string | null;
  email: string | null;
  status: Status;
  paused: boolean;
  folders: SyncedFolder[];
  drop_box: boolean;
  drop_box_path: string;
  organizer_dir: string;
  autostart: boolean;
  ignore: string[];
  waiting: number;
  device_name: string;
  on_demand: OnDemand;
};

export type Page = "sync" | "drop-box" | "settings";

/** The app's state, kept current by its `state` events. */
export function useView(): View | undefined {
  const [view, setView] = useState<View>();
  useEffect(() => {
    let live = true;
    void invoke<View>("app_state").then((v) => live && setView(v));
    const unlisten = listen<View>("state", (event) => setView(event.payload));
    return () => {
      live = false;
      void unlisten.then((stop) => stop());
    };
  }, []);
  return view;
}

/** Runs `onEvent` for each of the app's events of this name. */
export function useAppEvent<T>(name: string, onEvent: (payload: T) => void) {
  useEffect(() => {
    const unlisten = listen<T>(name, (event) => onEvent(event.payload));
    return () => void unlisten.then((stop) => stop());
    // oxlint-disable-next-line react-hooks/exhaustive-deps -- registered once
  }, [name]);
}

export const commands = {
  signIn: (server: string) => invoke<void>("sign_in", { server }),
  signOut: () => invoke<void>("sign_out"),
  pause: () => invoke<void>("pause"),
  resume: () => invoke<void>("resume"),
  addFolder: (remoteId: string, name: string, localPath?: string, mode: FolderMode = "sync") =>
    invoke<void>("add_folder", { remoteId, name, localPath: localPath || null, mode }),
  removeFolder: (id: number) => invoke<void>("remove_folder", { id }),
  moveFolder: (id: number, localPath: string) => invoke<void>("move_folder", { id, localPath }),
  setDropBox: (enabled: boolean) => invoke<void>("set_drop_box", { enabled }),
  setAutostart: (enabled: boolean) => invoke<void>("set_autostart", { enabled }),
  setIgnore: (patterns: string[]) => invoke<void>("set_ignore", { patterns }),
  openPath: (path?: string) => invoke<void>("open_path", { path: path ?? null }),
  unlockEncryption: (passphrase: string) => invoke<void>("unlock_encryption", { passphrase }),
  lockEncryption: () => invoke<void>("lock_encryption"),
};
