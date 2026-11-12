import { DropBoxReview } from "@/components/drop-box-review";
import { Page } from "../components/page";

/** The drop box's review, as on the web. */
export function DropBoxPage() {
  return (
    <Page title="Drop box">
      <DropBoxReview empty="Put files in the Drop box folder, and the AI proposes where they go." />
    </Page>
  );
}
