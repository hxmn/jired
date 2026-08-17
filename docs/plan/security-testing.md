# Security review & testing strategy

## Secret storage

- Primary: OS keychain via `keyring` (`service="jired"`, `user="<host>:<identity>"`);
  `session.db` stores only `secret_ref`.
- Headless fallback (D11): scrypt(`JIRED_SECRET_KEY`) → XChaCha20-Poly1305 blob in
  `session.db`. If the env var is absent, refuse unless `--insecure-store`
  (then 0600 plaintext with a loud warning).
- **Threat model of the fallback blob:** protects against casual file exfiltration
  and backups of `.jired/`; does **not** protect against an attacker holding the env
  var or same-user code execution. Stated verbatim in user docs.

## Filesystem & git hygiene

- `.jired/` mode 0700; `session.db` 0600 — enforced at open (warn + fix if wrong).
- `init` appends `.jired/` to `.gitignore` idempotently between sentinel comments.
  Attachment dot-folders are user content and committed by default
  (`[NEEDS CONFIRMATION]` #5).
- `.jired/lock`: advisory lock (`fs4`) + PID/timestamp; stale (dead PID) auto-broken;
  live lock → exit 8.

## Redaction rules

- All tokens held as `secrecy::SecretString` (no `Debug`, no `Serialize`) —
  type-level guarantee they can't reach `--json` output.
- `tracing` layer scrubs `Authorization`/`Cookie` headers and configured secret
  values from logs and error chains.
- Docs use `$JIRED_TOKEN` in every example, never literal tokens.
- `--insecure` TLS prints a red warning on every run.

## Testing strategy

| Layer | Approach |
|---|---|
| Unit | Config resolver precedence matrix; filename sanitizer; frontmatter parse/serialize; lock handling; exit-code mapping |
| Property-based (`proptest`) | Random `JiredAst` → MD → parse → AST-equal. Random ADF from node-type grammar → AST → back → byte-equal JSON (raw-block escape hatch for unknown nodes). Same for wiki markup. **Core investment.** |
| Recorded fixtures | `JiraApi` trait mocked by a fixture player; fixtures recorded by a `record` bin against real Cloud + DC 9.x docker, scrubbed of secrets. Cover: pagination, 429/Retry-After, stale-update, partial push failure, ADF vs wiki payloads |
| E2E CLI | `assert_cmd` + `insta` snapshots of stdout/stderr/exit codes for init→pull→edit→diff→push --dry-run→push against the fixture server; JSON validated against shipped schemas (`jsonschema`) |
| Snapshot | `insta` for every rendered `.md` and every `--dry-run` plan |

## CI matrix

- GitHub Actions: {ubuntu, macos, windows} × stable Rust; fmt + clippy -D warnings.
- Windows job runs symlink-fallback tests with **and** without symlink privilege.
- Weekly job re-verifies recorded fixtures against a live Cloud sandbox to catch
  API drift.

## Measured behaviour

`cargo test --test scale -- --ignored --nocapture`, 2000 synthetic issues, debug
build on one core:

| Operation | Time | Per issue |
|---|---|---|
| `pull` (cold) | 2.74 s | 1.4 ms |
| `pull` (nothing changed) | 0.44 s | 0.2 ms |
| `status` (clean) | 0.28 s | 0.1 ms |
| `status` (10% dirty) | 0.30 s | 0.2 ms |
| `diff` (clean) | 0.41 s | 0.2 ms |
| `push --dry-run` (10% dirty) | 0.50 s | 0.3 ms |

`state.db` + WAL: 4.1 MiB, about 2.1 KiB per issue — so a 10k-issue workspace is
roughly 20 MiB and a cold pull around 15 s. Comfortably inside the plan's
budget, which guessed 100–300 MiB.

Caveat: these issues are synthetic and smaller than real ones. An instance with
long ADF descriptions, many comments and heavy custom fields will store several
times more per issue; the *shape* of the cost (linear, dominated by conversion
and hashing) is what this measures.

## Top risks & mitigations

1. **Conversion fidelity (ADF)** — raw-block invariant caps blast radius (worst case
   "region is read-only", never corruption); property tests are the second line.
2. **DC variance** (versions, plugins, proxy base paths, self-signed TLS) —
   capability probe at `init` (`serverInfo`), D20 version floor, real-DC fixtures
   from day one.
3. **No server CAS** (D8 race) — minimize GET→PUT gap, re-verify `updated` in PUT
   response; documented limitation.
4. **Windows symlinks** — three-mode projector (symlink/hardlink/stub) built and
   CI-tested in M4, not bolted on.
5. **State/file drift** (git checkout, syncthing under jired) — hash-based detection;
   `status` always truthful; `state.db` deletable-and-refetchable by design.
