# NAS static publication runbook

This is an opt-in migration path, not a replacement for the current Compose project yet.
Keep the NAS HTTP service and tunnel running until the Pages cutover has been observed.
The script does not change DNS, create credentials, pull a new application image, or delete
the previous discovery cache.

## Before NAS access is available

Review the source PR stack and the
[reading-site infrastructure PR](https://github.com/michaelmvh/reading-site/pull/1).
Synthetic export, browser behavior, publisher validation, and scripted orchestration can
be exercised on a development machine. Actual Synology filesystem permissions, Docker
mounts, scheduler environment, SSH credentials, and live-provider scale still need rehearsal.

## One-time NAS setup

1. After merging reviewed source changes, use the successful GHCR build's immutable
   `sha-...` tag or digest. Do not use an old `latest` image that lacks export mode.
2. Install/configure Git, rsync, and GitHub CLI for the account that runs Task Scheduler.
   Set its Git author name/email for snapshot commits and configure SSH known hosts.
3. Clone `michaelmvh/reading-site` on `main` using a dedicated repository-scoped write
   deploy key. Keep that private key outside the checkout. Merge the infrastructure PR
   first; the checkout must contain `scripts/validate.ts` and the locked npm dependencies.
4. Authenticate GitHub CLI for workflow read access. `gh run watch` needs Actions read
   permission; an SSH deploy key alone cannot call the Actions API. Store credentials in
   the scheduler account's private configuration, never the publishing repository.
5. In the publishing repository, select GitHub Actions as the Pages source and enable
   `PAGES_DEPLOY_ENABLED=true` only when the preview snapshot is ready.
6. Place `static-compose.yml` and `publish-static.sh` together in the deployment directory.
   Configure the variables shown in `static.env.example` in a private scheduler wrapper.
   Do not commit that wrapper or contact/credential configuration.
   For OpenAlex, configure `GSRF_OPENALEX_API_KEY` using a verified free key from the provider.
   Full feeds require cursor paging, which is not available anonymously beyond the first
   page. The publisher checks that this setting is present before starting an OpenAlex run.
7. Create a dedicated exporter cache with `docker volume create reading-static-cache` and
   set `CACHE_VOLUME` accordingly. Do not share a writable cache with the still-running HTTP
   container: their snapshot refreshes can race. Start empty, or seed from a consistent
   backup while the old service is stopped; do not copy a live cache mid-write.
8. Choose `WORK_ROOT` on a volume with enough space for one export plus any failed attempts.
   It must be outside the publishing checkout. The exporter image uses its dedicated cache
   volume, while the Node validator uses only staged/public output and the publishing tools.

The application image is pulled deliberately before a manual first run:

```sh
/usr/local/bin/docker pull "$STATIC_IMAGE"
sh /path/to/publish-static.sh
```

Set `BOOTSTRAP_EXPORT=1` only for the first run when `site/` does not exist. Subsequent runs
require the previous public archive. A missing/corrupt archive is a recovery problem, not
permission to reset article identities. Do not set the bootstrap override permanently.

Use `SITE_URL=https://michaelmvh.com/reading-site/` for the pre-cutover project-site preview.
At the approved DNS cutover, switch to `https://reading.michaelmvh.com/` and regenerate.
No backwards-compatible `?rss` endpoint is required; update subscriptions to XML file URLs.

## Scheduling and notifications

Create a separate daily task named **Publish Reading Site**. Use the same account whose Git,
SSH, and GitHub CLI configuration was rehearsed. Set an explicit PATH and the variables in
the example; scheduled jobs do not inherit an interactive login shell's environment.
Configure Task Scheduler email/notification delivery for nonzero exits and inspect the
first notification deliberately. Do not redirect away stderr.

The existing weekly **Update Scholar RSS** task updates application images; it is not the
daily data-publication task. Keep it unchanged while the old HTTP service is the fallback.
After retirement, replace its procedure with a deliberate reviewed exporter-image update;
updating a pinned image requires changing `STATIC_IMAGE`, not just pulling `latest`.

The daily script locks publication, requires a clean main checkout, fast-forwards it,
exports into a fresh staging directory, validates the complete snapshot, stages only
`site/`, commits/pushes it, and waits for the matching Pages workflow and deploy job.
A successful push or a skipped deploy is not reported as a successful publication.
No-change runs skip the commit and workflow.

Publishing validation rejects a feed disappearing or losing more than 40% of its records.
For an intentional change, review the output and use `ALLOW_FEED_SHRINK=true` for that run
only. Required feeds must still contain records. Static export requires discovery refreshes
to succeed, while allowing incremental cache reuse and conditional 304 responses. A failed
required refresh leaves the previously deployed snapshot live; optional metadata enrichment
can fail with an explicit log without discarding otherwise usable records.

## Failure recovery

- Failed export/validation leaves the checkout and live site untouched, with staging data
  retained for inspection. Remove only the specific failed `run.*` directory after review;
  monitor disk usage if repeated failures accumulate.
- Failed copy/commit leaves a dirty checkout. Inspect it; the next run refuses to discard
  local edits. Restore from a known-good snapshot rather than resetting blindly.
- Failed push leaves an unpushed commit. Inspect and retry that commit's push before
  generating another snapshot.
- Failed/skipped deployment leaves a pushed snapshot but no successful new Pages deployment.
  Fix repository settings or rerun the failed workflow; the last successful site remains live.
- A lock left by power loss must be removed only after confirming no publication is active.

The script cleans up its own successful staging directory, never the checkout or cache.
Back up the previous published archive and private credential configuration separately.
Use an independent scheduled monitor for excessive staleness: it should alert if
`https://reading.michaelmvh.com/status.json` is unavailable or its `generated_at` is older than
48 hours. Task failure notifications alone cannot detect a powered-off NAS.

## Acceptance before retiring the tunnel

Rehearse first publication, an unchanged rerun, a data update, source/validation failure,
concurrent invocation, and a deployment failure. Inspect mounted file ownership after
the Docker runs. Verify RSS with TRMNL, icons on iOS, and direct article links at the final
domain. Keep at least one week of successful scheduled runs before retiring the old service.
