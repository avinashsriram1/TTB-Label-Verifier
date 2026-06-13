# Generated Label Test Pack

These labels are deterministic local PNG fixtures for testing the offline TTB verifier. They include compliant labels, expected failures, and OCR stress cases.

Use `manifest.csv` for the batch UI. The extra `expected_verdict` and `notes` columns are for humans; the app ignores unknown manifest columns.

For labels with embedded decorative artwork, use the `image-rich` subfolder. It
has its own `manifest.csv` and `manifest.json`.

For labels that should exercise V2 adaptive escalation, use the `hard-cases`
subfolder. Those fixtures intentionally include side warnings, low contrast,
glare, small dense warning text, and skewed/perspective-style layouts.

Recommended checks:

- `01_compliant_white_wine.png` should pass on easy wine fields.
- `02_compliant_bourbon.png` should pass proof/ABV equivalence.
- `03_compliant_honey_liqueur.png` should pass a small liqueur label.
- `04_compliant_tequila.png` should pass country inference from Mexico/application data.
- `05_compliant_side_warning_red_wine.png` is compliant but intentionally harder because the warning is sideways; adaptive mode may pass or route to review.
- `06_noncompliant_missing_warning.png` should fail government warning.
- `07_noncompliant_titlecase_warning.png` should fail all-caps heading.
- `08_noncompliant_wrong_abv.png` should fail alcohol content against the manifest.
- `09_noncompliant_missing_net_contents.png` should fail net contents against the manifest.
- `10_noncompliant_noisy_warning_review.png` should route to review because the warning heading is OCR-noisy.
