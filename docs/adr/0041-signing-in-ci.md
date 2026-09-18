# ADR-0041 — Signing in CI

**Status:** accepted (WWW-93)

## Context

ADR-0030 put the signing key on exactly two machines — the laptop and the Mac
mini — and made `paperctl sign-release` a manual, Mac-only command: "nothing
polls or triggers it," per that ADR's own decision, because CI is a shared,
disposable, internet-facing environment and a key GitHub can read reduces a
signature to "GitHub said so" rather than "the publisher said so" (§12).

Calum questioned that in this session, having previously shipped iOS apps
where CI holds the distribution key directly (`fastlane match` decrypts a
certificate + private key on the runner and calls `codesign` there). That is,
in fact, the industry default, not the exception: Android CI signs `.apk`s
with a keystore secret the same way, `npm publish` runs from an automation
token in CI, and plenty of production software ships with a static key sitting
in the CI provider's secret store the whole time. ADR-0030's stricter
never-touches-GitHub rule is the *unusual* choice, made deliberately for a
single-maintainer personal project, not something every signing setup is
required to do.

Before making the change, this session flagged the specific tension: Calum's
own memory record of generating this key states he considered and rejected a
private repo for it ("Calum asked whether a private repo would do; it will
not"), and a standing rule says trust-root work stops for his explicit
sign-off regardless of any other standing goal. Both were surfaced; a
self-hosted-runner alternative was offered that would have automated signing
in CI *without* the key ever leaving the Mac mini or reaching GitHub's
servers at all (register the mini as a GitHub Actions self-hosted runner;
`release.yml`'s sign step then executes locally, same trust boundary as
today, just no more manual command). Calum chose the plain GitHub Actions
secret instead, in full knowledge of the tradeoff and of his own prior
decision. This ADR records that as an informed reversal, not an oversight.

## Decision

**`PAPERCLIP_SIGNING_KEY`**, a repository secret on `0x63616c/paperclip`,
holds the raw contents of `~/.paperclip/paperclip.key`. Set once via
`gh secret set PAPERCLIP_SIGNING_KEY < ~/.paperclip/paperclip.key` (WWW-93);
never printed to a log, never committed.

**`release.yml`'s `app` and `platform` jobs each grow a `Sign` step**,
immediately after `Publish`, in the same job — not a separate downstream job —
so nothing has to wait on release visibility propagating and the app job can
pass `--archive` at the already-built `.paperpkg` instead of making
`sign-release` re-download what this job just uploaded. Each writes the
secret to `$RUNNER_TEMP` with `umask 077` (mode 600 from creation, matching
`paperctl key generate`'s own reasoning — ADR-0013), runs
`cargo run -p paperctl -- sign-release <tag> --key "$RUNNER_TEMP/paperclip.key"`
with no other flag changes, and deletes the key file immediately after.
`$RUNNER_TEMP` lives outside the checkout and outside anything a later step
in the same job would accidentally tar up or upload; the runner itself is
destroyed at job end regardless.

**Nothing about `paperctl sign-release` changes.** The rebuild-and-compare
refusal (ADR-0030) still runs by default — and now genuinely verifies
something in the common case, rather than needing `--trust-ci-build`, because
the GitHub runner already has the real `gcc-aarch64-linux-gnu` `release.yml`
installs a few steps earlier, the same toolchain that built the archive being
checked. `--trust-ci-build` remains for the cases ADR-0030 built it for:
someone signing by hand on a Mac with no cross GCC.

**A person can still sign by hand.** `sign-release --key <path>` never asked
where the key file came from; running it from the laptop or the mini with
`~/.paperclip/paperclip.key` works exactly as before. This ADR adds an
automatic path, it does not remove the manual one.

**ADR-0030 is not deleted.** Its Status line now says the never-touches-
GitHub rule is superseded here; everything else it decided — the archive
format assumptions, the rebuild-and-compare mechanism, why it refuses rather
than warns — is unchanged and is exactly what this ADR invokes from CI.

## What it costs

- **The signature now only proves "the repository's Actions secrets store
  said so."** ADR-0030's own framing of the risk is accurate and still true:
  anything with read access to Actions secrets on this repo — any workflow
  running on `main`, or a malicious dependency pulled into a build step
  before this one — can exfiltrate the key. On a single-maintainer repo with
  no third-party contributors and no PR-triggered workflows that run
  untrusted code, that surface is small, but it is not zero, and it does not
  shrink if the repo ever gains a second contributor or a workflow that
  builds a dependency from an untrusted source.
- **Every release signed from here on is only as trustworthy as GitHub's
  secret store and this repository's own workflow files.** A GitHub account
  compromise, not just a key-file leak, now reaches the trust root.
- **The key cannot be rotated by revoking a filesystem permission.** If it
  ever needs rotating, that means generating a new key, updating the secret,
  and re-signing or accepting that old releases stay signed under the old
  key — `paperctl key generate` already refuses to overwrite without
  `--force`, and that refusal now also protects the copy sitting in GitHub.

## What would make this wrong

- If this repository ever accepts external contributions or runs a workflow
  triggered by a fork's pull request, `pull_request_target` or an equivalent
  privilege-escalation surface reaches the secret, and this decision needs
  revisiting before that lands, not after.
- If GitHub Actions secrets are ever implicated in a real incident — a leaked
  secret, a compromised Action in the dependency chain — the self-hosted-
  runner alternative considered and declined here is the first thing to
  reconsider, since it keeps CI-triggered automatic signing while dropping
  this ADR's actual risk.
