# meshmon operator runbook

Short operational notes for running a meshmon deployment. This doc
grows as workflows mature; keep entries focused on what an operator
actually does rather than implementation detail.

## Exporting a path report as PDF

The Report page (`/report?source_id=…&target_id=…&from=…&to=…`) is
designed to print directly from a Chromium-based browser.

1. Open the report and click **Export PDF**.
2. In the browser print dialog, choose **Save as PDF** (or a physical
   printer) and pick the paper size you want — both **Letter** and
   **A4** are supported. The page layout adapts to whichever size the
   dialog selects.
3. Leave margins at the browser default; the report reserves its own
   15mm margin via `@page`.
4. Keep **Background graphics** enabled so status colors and borders
   print.

Only Chromium browsers (Chrome, Edge, Brave, …) are supported for
print; Firefox and Safari render the interactive view correctly but
aren't tested for PDF export.
