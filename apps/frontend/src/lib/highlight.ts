/** A piece of a search snippet: a matched word or the text around it. */
export type Piece = { text: string; match: boolean };

/**
 * A search snippet in pieces. The API marks the words it found between
 * U+0001 and U+0002; anything unbalanced is shown as plain text.
 */
export function splitHighlights(snippet: string): Piece[] {
  const pieces: Piece[] = [];
  const push = (text: string, match: boolean) => {
    if (!text) return;
    const last = pieces.at(-1);
    if (last && last.match === match) last.text += text;
    else pieces.push({ text, match });
  };
  let rest = snippet;
  while (rest) {
    const start = rest.indexOf("\u0001");
    if (start === -1) {
      push(rest.replaceAll("\u0002", ""), false);
      break;
    }
    push(rest.slice(0, start).replaceAll("\u0002", ""), false);
    const after = rest.slice(start + 1);
    const end = after.indexOf("\u0002");
    if (end === -1) {
      push(after.replaceAll("\u0001", ""), false);
      break;
    }
    push(after.slice(0, end).replaceAll("\u0001", ""), true);
    rest = after.slice(end + 1);
  }
  return pieces;
}
