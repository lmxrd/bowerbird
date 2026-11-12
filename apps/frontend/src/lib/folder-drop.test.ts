import { describe, expect, it } from "vitest";
import { collectEntries, fromFile, tooMany, topFolders } from "./folder-drop";

/** A dragged file, as `webkitGetAsEntry` gives it. */
function file(name: string) {
  return {
    name,
    isFile: true,
    isDirectory: false,
    file: (resolve: (f: File) => void) => resolve(new File([name], name)),
  };
}

/** A dragged folder whose reader gives its children `per` at a time. */
function folder(name: string, children: unknown[], per = 2) {
  return {
    name,
    isFile: false,
    isDirectory: true,
    createReader: () => {
      let at = 0;
      return {
        readEntries: (resolve: (batch: unknown[]) => void) => {
          resolve(children.slice(at, at + per));
          at += per;
        },
      };
    },
  };
}

describe("folder drop", () => {
  it("reads a dragged folder all the way down, every batch of it", async () => {
    const entries = [
      folder("Japan trip", [
        file("plan.txt"),
        folder("tickets", [file("train.txt"), file("plane.pdf"), file("bus.txt")]),
        file("map.png"),
      ]),
      file("note.txt"),
    ];
    const dropped = await collectEntries(entries as unknown as FileSystemEntry[]);
    expect(dropped.map((d) => d.path)).toEqual([
      "Japan trip/plan.txt",
      "Japan trip/tickets/train.txt",
      "Japan trip/tickets/plane.pdf",
      "Japan trip/tickets/bus.txt",
      "Japan trip/map.png",
      "note.txt",
    ]);
    expect(dropped[1]?.file.name).toBe("train.txt");
  });

  it("gives an empty folder no files", async () => {
    const dropped = await collectEntries([folder("Empty", [])] as unknown as FileSystemEntry[]);
    expect(dropped).toEqual([]);
  });

  it("names a chosen file by its path in the chosen folder", () => {
    const plain = new File(["a"], "a.txt");
    expect(fromFile(plain).path).toBe("a.txt");
    const chosen = new File(["b"], "b.txt");
    Object.defineProperty(chosen, "webkitRelativePath", { value: "Trip/b.txt" });
    expect(fromFile(chosen).path).toBe("Trip/b.txt");
  });

  it("lists the folders at the top of a drop", () => {
    const at = (path: string) => ({ file: new File([], "x"), path });
    expect(topFolders([at("a.txt"), at("Trip/x/b.txt"), at("Trip/c.txt"), at("Work/d")])).toEqual([
      "Trip",
      "Work",
    ]);
    expect(topFolders([at("a.txt")])).toEqual([]);
  });
});

describe("drop size", () => {
  it("refuses more than 1,000 files", () => {
    expect(tooMany(1000)).toBeUndefined();
    expect(tooMany(1001)).toBe("That is 1,001 files: drop at most 1,000 at a time");
  });
});
