# W0 manifest policy

`g0-scope.json` records observed toolchain versions and honest readiness states. It does not contain media, model, or binary payloads.

Every later binary, fixture, model, or voice entry must record source, version, license, architecture, size, SHA-256, and runtime requirements before it can be `PASS`. Missing required artifacts are `BLOCKED`; missing optional AI/GPU/Android capability is `UNAVAILABLE`.

Validate a record with:

```text
node scripts/evidence/validate-record.mjs scripts/manifest/g0-scope.json
```
