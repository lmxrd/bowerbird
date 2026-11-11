import { describe, expect, it } from "vitest";
import { inlineType, largeLooseFiles, PLAIN_UPLOAD_MAX } from "./transfer";

/** A file of a given size, without allocating it. */
function sized(name: string, size: number): File {
  const file = new File([""], name);
  Object.defineProperty(file, "size", { value: size });
  return file;
}

describe("largeLooseFiles", () => {
  it("picks loose files too large for a multipart drop", () => {
    const big = { file: sized("film.mov", PLAIN_UPLOAD_MAX + 1), path: "film.mov" };
    const small = { file: sized("a.txt", 10), path: "a.txt" };
    const inFolder = { file: sized("b.mov", PLAIN_UPLOAD_MAX + 1), path: "Trip/b.mov" };
    expect(largeLooseFiles([big, small, inFolder])).toEqual([big]);
  });
});

describe("inlineType", () => {
  it("shows only what cannot run as a page", () => {
    expect(inlineType("Taxes.PDF")).toBe("application/pdf");
    expect(inlineType("photo.jpeg")).toBe("image/jpeg");
    expect(inlineType("page.html")).toBeUndefined();
    expect(inlineType("drawing.svg")).toBeUndefined();
    expect(inlineType("pdf")).toBeUndefined();
  });
});
