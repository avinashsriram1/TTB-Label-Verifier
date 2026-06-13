# Samples

The manifests show the supported batch formats.

V1 keeps sample image generation separate from the application binary so the verifier
does not depend on image-generation tooling at runtime. Any PNG/JPG/TIFF label image
whose filename matches the manifest can be used with these files.

Recommended quick test labels:

- front image containing brand, class/type, ABV, and net contents
- back image containing the full `GOVERNMENT WARNING` statement
- failure image with `Government Warning` in title case
- failure image with the warning omitted

Generated offline test labels are in `samples/generated-labels`. That pack includes
compliant labels, warning failures, field mismatch failures, a sideways warning
case, and CSV/JSON manifests for batch upload testing.
