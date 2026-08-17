# Security

## Where your credential lives

jired never writes a token into a config file. Where it does go depends on what
the machine offers:

| Storage | When | What protects it |
|---|---|---|
| OS keychain | Default, whenever one is reachable | The keychain. `.jired/session.db` holds only a reference, not the secret. |
| Encrypted blob | No keychain, and `JIRED_SECRET_KEY` is set | scrypt-derived key, XChaCha20-Poly1305. The blob lives in `session.db`. |
| Plaintext | No keychain, no key, and you passed `--insecure-store` | Nothing but file permissions. jired says so on every run that stores one. |

Without a keychain and without `JIRED_SECRET_KEY`, jired refuses to store the
credential rather than quietly falling back to plaintext.

```sh
# Headless or CI:
export JIRED_SECRET_KEY='a passphrase this machine can produce'
jired init --url https://acme.atlassian.net --email you@acme.com --token "$JIRA_TOKEN"
```

## Threat model of the encrypted blob

Be precise about what this buys, because "encrypted" invites the wrong
assumption.

**It protects against:** someone reading `.jired/session.db` out of a backup, a
snapshot, a container image, or a repository it was committed to by mistake.
The file on its own is useless without the passphrase.

**It does not protect against:** anyone who can also read `JIRED_SECRET_KEY` —
which means anyone who can read the environment of the process, run code as your
user, or read the CI configuration that sets it. On a single-user machine where
the key is exported in a shell profile, an attacker who reaches the file has
almost certainly reached the key too.

So the blob is a meaningful improvement over plaintext for data *at rest and
away from the machine*, and close to no improvement against an attacker already
on it. If the machine has a keychain, use it — that is the default for a reason.

The key derivation is scrypt with `log_n = 14` (about 16 MiB), `r = 8`, `p = 1`,
with a fresh 16-byte salt and 24-byte nonce per credential, so two identical
tokens do not produce identical ciphertext.

## What init writes

`jired init` adds this block to `.gitignore`, editing only between the
sentinels so your own entries survive regeneration:

```gitignore
# BEGIN jired
# jired machine state: credentials, sync database, logs.
/.jired/
# END jired
```

`.jired/` is created mode `0700` and `session.db` mode `0600`. The mode is
re-checked and repaired on every open, so a careless `chmod` is corrected rather
than tolerated.

Attachment folders (`.PROJ-142. Summary/`) are **not** ignored: they hold issue
content, and whether that belongs in your repository is your call.

## What never appears in output

Tokens are held in `secrecy::SecretString`, which has no `Debug` and no
`Serialize`, so there is no path from a credential to `--json` output at all.
The `Authorization` header is marked sensitive, so even a debug-formatted header
prints `Sensitive` rather than its value.

Three tests enforce this rather than leaving it to review
(`tests/no_secrets.rs`): a full workflow is run at `trace` verbosity with a
known token and the output is searched for it; the committed fixtures are
scanned for credentials, real hostnames and signed URLs; and the guard itself is
verified by injecting a leak and confirming it fails.

## Recording fixtures from a real instance

`jired-record` scrubs by *field name* rather than by pattern-matching values,
because a pattern that almost matches is how a credential reaches a public
repository. Identities become stable pseudonyms so a fixture stays internally
coherent, the instance hostname and the token are redacted as literals wherever
they appear, and avatar and `self` URLs are dropped.

Review the output before committing it. Scrubbing cannot know that
`customfield_10420` holds a customer name.

## TLS

For a Data Center instance with a private CA:

```sh
jired --ca-cert /etc/ssl/corp-ca.pem status
```

`--insecure` disables verification entirely and prints a warning on **every**
run, not just the one that configured it — a workspace set up that way months
ago is still exposed today.

## Reporting a problem

Open an issue describing the impact rather than a proof of concept, and please
do not include a real token in the report.
