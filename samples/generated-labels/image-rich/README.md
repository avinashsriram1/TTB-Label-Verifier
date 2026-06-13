# Image-Rich Generated Label Test Pack

These deterministic PNG fixtures contain decorative artwork inside the label area so OCR has to handle more realistic visual noise. They are useful for testing labels similar to imported wine panels, illustrated honey liqueur labels, tall red liqueur labels, and side-panel warning layouts.

Use `manifest.csv` in the Batch page. The app ignores the human-only `expected_verdict` and `notes` columns.

Recommended checks:

- `11_compliant_illustrated_rose_wine.png` should pass or expose any wine/country weakness around decorative layouts.
- `12_compliant_illustrated_honey_liqueur.png` should pass proof/ABV and warning detection with honeycomb/bear artwork.
- `13_compliant_illustrated_pomegranate_liqueur.png` is intentionally low contrast and may pass or route to review.
- `14_noncompliant_illustrated_missing_warning.png` should fail government warning.
- `15_noncompliant_illustrated_titlecase_warning.png` should fail all-caps warning heading.
- `16_review_illustrated_side_panel_cider.png` should pass or route to review depending on side-panel OCR quality.
