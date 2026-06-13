# Hard Generated Label Test Pack

These deterministic PNG fixtures are intentionally difficult. They are meant to exercise the V2 adaptive path rather than the V1-style fast path alone.

Use `manifest.csv` in the Batch page. The app ignores `expected_verdict`, `expected_path`, and `notes`; those columns are for test planning.

Expected behavior:

- Most labels should require `cheap_repair`, `enhanced_retry`, or a bounded `timeout_review` path.
- They should still finish under the configured per-image budget.
- None of these cases should leak raw OCR into observed field values.

Fixtures:

- `17_hard_side_warning_dark_wine.png`: dark wine label with the warning only in a vertical side strip.
- `18_hard_low_contrast_gold_lager.png`: low-contrast lager label with noisy texture.
- `19_hard_glare_tequila.png`: agave label with simulated camera glare over text.
- `20_hard_curved_warning_small_liqueur.png`: decorative red liqueur with small warning text near a barcode.
- `21_hard_perspective_style_bourbon.png`: skewed/perspective-style bourbon label.
