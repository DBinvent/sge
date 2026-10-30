# Trivy: Schema Guard

The nightly's scan (yt-cluster-tests nightly/nightly.sh, `security_report`), run 20261030-140648, 2026-10-30 17:46 UTC:
Trivy 0.69.3 on Schema Guard's crates - the lock file of the source the run tested - and on the
image `dbinvent/sge:latest` as Docker Hub serves it: every known vulnerability counted by severity, the CRITICAL
and HIGH ones listed.

Software bills of materials, CycloneDX: [sbom-sge.cdx.json](sbom-sge.cdx.json) (the crates),
[sbom-sge-image.cdx.json](sbom-sge-image.cdx.json) (the image).

## The crates

```text

Report Summary

┌────────────┬───────┬─────────────────┐
│   Target   │ Type  │ Vulnerabilities │
├────────────┼───────┼─────────────────┤
│ Cargo.lock │ cargo │        0        │
└────────────┴───────┴─────────────────┘
Legend:
- '-': Not scanned
- '0': Clean (no security findings detected)

Report Summary

┌────────────┬───────┬─────────────────┐
│   Target   │ Type  │ Vulnerabilities │
├────────────┼───────┼─────────────────┤
│ Cargo.lock │ cargo │        0        │
└────────────┴───────┴─────────────────┘
Legend:
- '-': Not scanned
- '0': Clean (no security findings detected)
```

## The image `dbinvent/sge:latest`

```text

Report Summary

┌──────────────────────────────────────────┬────────┬─────────────────┐
│                  Target                  │  Type  │ Vulnerabilities │
├──────────────────────────────────────────┼────────┼─────────────────┤
│ dbinvent/sge:latest (alpine 3.23.7)      │ alpine │        0        │
├──────────────────────────────────────────┼────────┼─────────────────┤
│ usr/share/doc/schema-guard/sbom.cdx.json │ cargo  │        0        │
└──────────────────────────────────────────┴────────┴─────────────────┘
Legend:
- '-': Not scanned
- '0': Clean (no security findings detected)

Report Summary

┌──────────────────────────────────────────┬────────┬─────────────────┐
│                  Target                  │  Type  │ Vulnerabilities │
├──────────────────────────────────────────┼────────┼─────────────────┤
│ dbinvent/sge:latest (alpine 3.23.7)      │ alpine │        0        │
├──────────────────────────────────────────┼────────┼─────────────────┤
│ usr/share/doc/schema-guard/sbom.cdx.json │ cargo  │        0        │
└──────────────────────────────────────────┴────────┴─────────────────┘
Legend:
- '-': Not scanned
- '0': Clean (no security findings detected)
```
