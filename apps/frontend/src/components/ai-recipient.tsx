import type { Destination, Recipient } from "organizer-sdk/types";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";

/** Where one kind of request goes: its endpoint, then the model. */
function To({ what, endpoint, model }: { what: string; endpoint: string; model: string }) {
  return (
    <li>
      {what} go to <code>{endpoint}</code> ({model}).
    </li>
  );
}

const named = (d: Destination) => `${d.name}, model ${d.model}`;

/** What the drop box and search send to the AI, and where each model's requests go. */
export function AiRecipient({ recipient }: { recipient: Recipient }) {
  const embeddings = recipient.embeddings_model !== "";
  const { images, scans, audio } = recipient;
  return (
    <Card>
      <CardHeader>
        <CardTitle>What Organizer sends to the AI</CardTitle>
        {!recipient.enabled && (
          <CardDescription>
            No AI model is set up: items wait in the drop box, and you can place them by hand.
          </CardDescription>
        )}
      </CardHeader>
      {recipient.enabled && (
        <CardContent className="flex flex-col gap-3 text-sm">
          <ul className="flex list-disc flex-col gap-1 pl-5">
            <li>
              For every item: its name, type, size and dates, the note you add when you drop it, and
              the names of folders in your tree (the top level and the ones most like the item).
            </li>
            <li>
              When reading contents is on: the text taken from the file (PDF, office documents,
              plain text), up to the first 32,000 characters, and for a link the page's title and
              text.
            </li>
            {images && (
              <li>
                When reading contents is on, photos and images (JPEG, PNG, WebP, GIF) themselves, up
                to 20 MB, for their text and a line on what they show; for the drop box and search.
              </li>
            )}
            {scans && (
              <li>
                When reading contents is on, the page images of PDFs without text (up to 20 pages),
                for their text; for the drop box and search.
              </li>
            )}
            {audio && (
              <li>
                When reading contents is on, recordings themselves, up to 100 MB, to be transcribed;
                their first hour is kept, for the drop box and search.
              </li>
            )}
            {embeddings && (
              <li>
                With embeddings on: your folder names, and the text read from your files for search
                (when reading contents is on), to compare them by meaning.
              </li>
            )}
            <li>
              Never: files a sensitive-file rule holds by their name or path (only their name, or
              nothing, as the rule says), the bytes of other files, your account or other users'
              files. A keyword rule can only look at what a model read of a photo, scan or
              recording.
            </li>
          </ul>
          <ul className="flex flex-col gap-1 text-muted-foreground">
            <To
              what="Proposals"
              endpoint={recipient.endpoint}
              model={`${recipient.provider}, model ${recipient.model}`}
            />
            {embeddings && (
              <To
                what="Embeddings"
                endpoint={recipient.embeddings_endpoint}
                model={`model ${recipient.embeddings_model}`}
              />
            )}
            {images && (
              <To what="Photos and images" endpoint={images.endpoint} model={named(images)} />
            )}
            {scans && <To what="Scanned pages" endpoint={scans.endpoint} model={named(scans)} />}
            {audio && <To what="Recordings" endpoint={audio.endpoint} model={named(audio)} />}
          </ul>
        </CardContent>
      )}
    </Card>
  );
}
