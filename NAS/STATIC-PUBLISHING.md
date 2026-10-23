# JSON-only NAS publication: DSM walkthrough

The NAS collects and publishes article **JSON**, not HTML or RSS. `reading-site` owns the
reader UX and builds the website in GitHub Actions. Site URLs, styles, and templates are
reader settings; updating them does not require changing the NAS image or collecting again.

The one-shot publisher image contains Rust, Node.js for JSON validation/orchestration, and
Git. No SSH service, host Git/Node/rsync/GitHub CLI installation, incoming port, or Docker
socket mount is needed. Keep the existing reader, tunnel, and weekly **Update Scholar RSS**
task unchanged until the replacement has completed its observation period.

## 0. Prerequisites and changes from the earlier draft

Review and merge the consolidated collector/publisher and reader PRs. Wait for the source
repository's successful **Publish Docker image** run to publish:

```text
ghcr.io/michaelmvh/scholarly-rss-feed-publisher:latest
```

Use the reviewed immutable `sha-...` tag in Compose where possible. Make the GHCR publisher
package public for anonymous NAS pulls if necessary. The original `scholarly-rss-feed`
image remains the HTTP server, independently of the new publisher.

The reader repository's main branch must contain `scripts/validate-data.ts`, its schema,
and its TypeScript site build. Earlier prebuilt-HTML deployment drafts are not sufficient.
GitHub Pages should use **GitHub Actions**, with publication disabled until the preview run.

If you prepared configuration from the earlier instructions, **remove `SITE_URL` from
publisher.env**. The publisher now rejects it. Set the reader repository's Actions variable
`READER_SITE_URL` instead; the initial default is `https://michaelmvh.com/reading-site/`.
Do not reuse an old `site/data/archive.json` as a new snapshot. No legacy-output migration
is implicit; the initial JSON data publication starts with the explicit bootstrap flag.

## 1. Prepare credentials privately

Obtain a verified free [OpenAlex API key](https://openalex.org/settings/api-key). Larger
feeds require authenticated cursor paging beyond the first page.

Create a GitHub fine-grained personal access token with access to **only `reading-site`**,
repository **Contents: Read and write**, and **Actions: Read-only**. Choose an expiration
you can renew. Workflow-editing, administration, SSH, and registry permissions are not needed.
The token is repository-scoped, not technically limited to the `data/` path.

Create a plain UTF-8 `secrets.env` file, using `NAS/secrets.env.example`:

```dotenv
PUBLISH_GITHUB_TOKEN=YOUR_GITHUB_TOKEN
GSRF_OPENALEX_API_KEY=YOUR_OPENALEX_KEY
```

Do not share these values in chat/PRs, commit them, or put them in an image or task command.
Compose mounts the file read-only. It is parsed by the publisher, not injected through
`env_file`. Both keys are required; unrelated settings belong in `publisher.env`.
Read-only mounting does not isolate secrets from other trusted code in the same container.

## 2. Prepare the File Station folder

Create a separate folder under the existing Docker shared folder:

```text
/volume1/docker/reading-publisher/
    static-compose.yml
    publisher.env
    secrets.env
    state/
```

Adjust the volume if your Docker share is elsewhere. Restrict this new folder and its
credentials to your administrator/Container Manager access; do not change the existing
reader's folder or enable public sharing.

Upload `NAS/static-compose.yml`. Copy `NAS/publisher.env.example` to **publisher.env**:

```dotenv
PUBLISH_REPOSITORY=michaelmvh/reading-site
PUBLISH_MODE=prepare
BOOTSTRAP_EXPORT=true
ALLOW_FEED_SHRINK=false
GSRF_MAILTO=YOUR_CONTACT_EMAIL
PUBLISH_GIT_NAME=Reading Site Publisher
PUBLISH_GIT_EMAIL=YOUR_GITHUB_NOREPLY_EMAIL
```

Use a real contact email for OpenAlex and, if desired, your GitHub noreply address for
public commit attribution. This is an environment file, not a shell script.

The publisher clones the repository itself into `state/checkout`, uses a dedicated cache,
and reads the previous committed `data/` on subsequent runs. It must not share the old HTTP
container's writable cache. Root with only `DAC_OVERRIDE` allows access to private
administrator-owned bind mounts; rootfs and credentials remain read-only and no unrelated
NAS directories are mounted.

## 3. Prepare locally without publishing

Use **Container Manager -> Project -> Create**, selecting this folder and Compose file.
Create/start the publisher project. An exited container with code **0** is normal: it is
a batch job, not a web server.

In prepare mode, the container fetches publications, writes a complete JSON snapshot, and
validates it against the reader's schema. It does not commit, push, build HTML, or deploy.
Inspect Container Manager logs and:

```text
state/last-run.json
state/work/run-.../data/snapshot.json
```

The run summary should say `"status": "prepared"`. Data lives under `articles/<bucket>.json`
with checksums/counts in `snapshot.json`; there should be no HTML, RSS, or UI assets there.
Full-feed collection needs the private OpenAlex key. Do not bypass errors or coverage checks.

## 4. Create a disabled daily task

In **Control Panel -> Task Scheduler**, create a **Scheduled Task -> User-defined script**:

- Name: **Publish Reading Site**.
- User: the account allowed to run Docker, normally **root** for this setup.
- Schedule: daily at your chosen time; leave disabled initially.
- Enable saved output and abnormal-exit notifications, and configure DSM email delivery.

Script, with your actual volume/folder:

```sh
cd /volume1/docker/reading-publisher &&
/usr/local/bin/docker compose -f static-compose.yml run --rm publisher
```

Only the host's existing Docker CLI is used. Every invocation loads the current configuration
and secret file. A container-owned lock prevents overlap with manual Container Manager runs.

## 5. Publish the preview data and let GitHub build the site

In the reader repository, keep `READER_SITE_URL=https://michaelmvh.com/reading-site/` and
set `PAGES_DEPLOY_ENABLED=true` when ready. Change `PUBLISH_MODE=publish` in the NAS file,
keep `BOOTSTRAP_EXPORT=true` for this first publication, and manually **Run** the disabled task.

The container commits only `data/`, preserves reader source, and waits for the resulting
reader build/deploy job. A push alone, skipped deployment, or failed build is not success.
Concurrent UI commits are fast-forwarded before the data is staged and the current schema
is rechecked. Conflicts or a later push race fail explicitly; no force-push is used.

Confirm `"status": "deployed"` in the run summary, then open:

```text
https://michaelmvh.com/reading-site/
https://michaelmvh.com/reading-site/feed.xml
```

GitHub Actions, not the NAS, generated these pages and XML. They should remain usable with
the NAS offline. Verify filters, article links, icons, and RSS without changing production DNS.

## 6. Enable scheduling and independent monitoring

After a successful first publication, change `BOOTSTRAP_EXPORT=false`, run again, and
enable the daily task. Keep `ALLOW_FEED_SHRINK=false`; investigate removed feeds or a record
count drop greater than 40%. Use its override only for a reviewed intentional change.
Retained article removal needs an explicit data migration even with that override.

Enable reader-repository monitoring with:

```text
STATUS_MONITOR_ENABLED=true
STATUS_URL=https://michaelmvh.com/reading-site/status.json
MAX_SNAPSHOT_AGE_HOURS=48
```

Verify GitHub Actions failure notifications. The monitor checks **data_generated_at**, not
the website's **built_at**, so UI-only rebuilds cannot conceal stale NAS data.

## 7. Move the subdomain after preview acceptance

Register/verify `reading.michaelmvh.com` in the reader's Pages settings. Set the reader
repository variable `READER_SITE_URL=https://reading.michaelmvh.com/` and manually run its
build workflow using the already committed JSON. **No NAS config or data refresh is needed.**

Coordinate that build with changing only `reading` to a DNS-only CNAME targeting
`michaelmvh.github.io`. Verify HTTPS, listings, article paths, icons, and RSS. Update
subscriptions and `STATUS_URL` to the final hostname. Leave apex, `www`, mail, and unrelated
tunnels unchanged. DNS/certificate delays mean this is not guaranteed zero downtime.

Keep the old NAS reader and its tunnel as a fallback through at least one week of successful
scheduled updates. Retire them only afterwards. The existing weekly image-update task must
be revised separately; it is not the daily JSON-publication task.

## Recovery

Failed export/validation leaves the public site unchanged. Failed staging directories are
kept under their specific `state/work/run-...` paths. Inspect them before deleting only that
attempt, and monitor disk space during repeated failures.

Dirty checkouts and unpushed commits are never reset automatically. Failed promotion retains
previous data in that attempt's `previous-data` directory. Inspect the checkout or failed
GitHub workflow before retrying. Recovery Git commands can run in the publisher image through
a temporary DSM task; SSH need not be enabled.

After power loss, remove `state/publisher.lock` only after confirming no publisher is running.
Back up committed data and credentials separately. Replace expired values in `secrets.env`;
the next one-shot container reads them again.

For reader regressions, restore reader source and rebuild with compatible data. For bad
article data, restore a known-good JSON revision and rebuild. Keep schema compatibility in
mind. Rollback to the NAS hostname/tunnel requires restoring old RSS URLs manually.
