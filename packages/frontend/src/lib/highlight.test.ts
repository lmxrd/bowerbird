import { describe, expect, it } from "vitest";
import { splitHighlights } from "./highlight";

describe("splitHighlights", () => {
  it("splits the marked words from the text around them", () => {
    expect(splitHighlights("the \u0001electricity\u0002 bill for \u0001May\u0002")).toEqual([
      { text: "the ", match: false },
      { text: "electricity", match: true },
      { text: " bill for ", match: false },
      { text: "May", match: true },
    ]);
  });

  it("keeps a snippet without marks as it is", () => {
    expect(splitHighlights("plain text")).toEqual([{ text: "plain text", match: false }]);
    expect(splitHighlights("")).toEqual([]);
  });

  it("joins marks next to each other and drops empty ones", () => {
    expect(splitHighlights("\u0001a\u0002\u0001b\u0002\u0001\u0002 c")).toEqual([
      { text: "ab", match: true },
      { text: " c", match: false },
    ]);
  });

  it("shows unbalanced marks as plain text", () => {
    expect(splitHighlights("a \u0001b")).toEqual([{ text: "a b", match: false }]);
    expect(splitHighlights("a\u0002 b")).toEqual([{ text: "a b", match: false }]);
  });
});
