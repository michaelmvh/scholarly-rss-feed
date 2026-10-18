# Container-only NAS publication: DSM walkthrough

The publisher is a separate, one-shot container. It includes the existing Rust exporter,
Node.js validation runtime, and Git. GitHub Actions is polled through its REST API, so no
GitHub CLI is needed. The NAS host needs only Container Manager and Task Scheduler:
**do not enable SSH or install Git, Node, rsync, or GitHub CLI on the NAS.**

Keep the existing reader, Cloudflare tunnel, and **Update Scholar RSS** task unchanged
throughout preparation. This guide supersedes the earlier host-based publication script.
The new publisher has no listening ports and no Docker socket mount.

## 0. Repository prerequisites

Review and merge the source PR stack, including the container-only publisher, and the
[reading-site publishing PRs](https://github.com/michaelmvh/reading-site/pulls).
Do not use the old server image to run the publisher. A successful **Publish Docker image**
workflow must produce:

```text
ghcr.io/michaelmvh/scholarly-rss-feed-publisher:latest
```

The workflow also produces immutable `sha-...` tags. Prefer the reviewed immutable tag in
`static-compose.yml` for deployment. The original `scholarly-rss-feed` image remains the
HTTP server; both images reuse the same Rust build.

Make the new GHCR publisher package public if it is private after its first publication,
so the NAS can pull it without an additional registry credential. The GitHub Pages source
must be **GitHub Actions**. Keep `PAGES_DEPLOY_ENABLED=false` until step 5.

No image is available merely because a draft PR exists; wait for its merged build.

## 1. Prepare two private credentials

Obtain a verified free [OpenAlex API key](https://openalex.org/settings/api-key).
Cursor paging beyond the first page requires a key; an email/contact address alone is
insufficient.

Create a GitHub **fine-grained personal access token**, owned by the account that owns
`reading-site`, with access to **only `reading-site`**:

- Repository **Contents: Read and write** (clone and push generated output).
- Repository **Actions: Read-only** (inspect the publication workflow and deploy result).
- Metadata read access is included automatically.

Choose an expiration you can maintain and arrange a renewal reminder. No workflow-editing,
administration, SSH key, or NAS login permission is required. The publisher does not change
GitHub variables or DNS on your behalf.

Save each credential as a separate plain UTF-8 text file, containing only the value:

```text
github-token.txt
openalex-api-key.txt
```

Do not paste either value into a PR/chat or include it in an image, Compose YAML, task
command, or `publisher.env`. The files are mounted read-only as Compose secrets.

## 2. Create the DSM folders and configuration

In **File Station**, create a separate folder under the existing `docker` shared folder:

```text
/volume1/docker/reading-publisher/
    static-compose.yml
    publisher.env
    secrets/
        github-token.txt
        openalex-api-key.txt
    state/
```

Adjust `/volume1` if your Docker shared folder is on another volume. Use a separate folder
from the existing reader. Restrict the folder and especially `secrets/` to the administrator
and Container Manager account; do not expose it through a public share.

Upload `NAS/static-compose.yml` from the reviewed source revision. Copy
`NAS/publisher.env.example` to a file named **publisher.env**, then edit:

```dotenv
PUBLISH_REPOSITORY=michaelmvh/reading-site
SITE_URL=https://michaelmvh.com/reading-site/
PUBLISH_MODE=prepare
BOOTSTRAP_EXPORT=true
ALLOW_FEED_SHRINK=false
GSRF_MAILTO=YOUR_CONTACT_EMAIL
PUBLISH_GIT_NAME=Reading Site Publisher
PUBLISH_GIT_EMAIL=YOUR_GITHUB_NOREPLY_EMAIL
```

Use your GitHub noreply email for commit attribution if you do not want a personal address
in the publishing repository's commit history. This is a Compose environment file, not
a shell script: do not add `export` or shell commands.

The state directory holds the publishing checkout, an isolated discovery cache, npm cache,
and staging output. The publisher clones `reading-site` itself. You do not clone it on the
NAS manually, and you must not share the old reader's writable cache.

## 3. Run a prepare-only container

In **Container Manager -> Project -> Create**, select the new folder and its Compose file.
Create/start the project. Its `publisher` service will fetch, generate, and validate a
snapshot, then exit. An exited container with exit code **0** is normal for this job.

`PUBLISH_MODE=prepare` makes no commits, pushes, or deployments. Successful logs end with
**Nothing was committed, pushed, or deployed**. In File Station, inspect:

```text
state/last-run.json
state/work/run-.../site/status.json
```

`last-run.json` must say `"status": "prepared"`. Its `output` field identifies the generated
site directory. This is local preparation, not a live browser preview yet.

If it fails, inspect Container Manager logs and `state/last-run.json`. Common setup issues
are an unavailable/private image, a missing infrastructure PR, file permissions, an
unverified/expired key, provider limits, or invalid feed output. Do not disable validation
or delete the old reader to work around an error.

Do not schedule prepare mode repeatedly: it is for setup and troubleshooting.

## 4. Create a disabled Task Scheduler job

In **Control Panel -> Task Scheduler**, create a **Scheduled Task -> User-defined script**:

- Name: **Publish Reading Site**.
- User: the account permitted to run Docker; on this NAS setup, normally **root**.
- Schedule: daily at your preferred time.
- Leave the task disabled until the manual publication succeeds.
- Enable abnormal-exit notifications and saved task output; configure DSM email delivery
  if it is not already configured.

Use this script, adjusting the volume/path if necessary:

```sh
cd /volume1/docker/reading-publisher &&
/usr/local/bin/docker compose -f static-compose.yml run --rm publisher
```

This uses only the Docker CLI already installed by Container Manager. Compose reads the
current `publisher.env` on every task run. Nothing requires an interactive login shell,
host Git credentials, or SSH access.

The container owns its lock, so a Task Scheduler run cannot race another publisher started
through Container Manager. A lock error should be investigated, not suppressed.

## 5. Publish the project-site preview

In GitHub's `reading-site` repository, set the Actions repository variable
`PAGES_DEPLOY_ENABLED=true`. Then edit `publisher.env` through File Station:

```dotenv
PUBLISH_MODE=publish
```

Keep the preview `SITE_URL` and initial `BOOTSTRAP_EXPORT=true`. Select the disabled
**Publish Reading Site** task and use **Run** to run it manually.

The publisher regenerates and validates the snapshot, updates only the checkout's `site/`
directory, commits/pushes it, and waits for the corresponding Pages **deploy** job.
A pushed commit or skipped deployment is not reported as success.

Confirm `state/last-run.json` says `"status": "deployed"`, then open:

```text
https://michaelmvh.com/reading-site/
https://michaelmvh.com/reading-site/feed.xml
```

The project site inherits the account's existing website domain. Its output uses the
`/reading-site/` path prefix until the final domain cutover.

## 6. Verify and enable daily publication

Check the listing, article pages, author/date/source filters, RSS, favicon, and iOS icon.
Run the job again and confirm existing articles/identities remain intact.

Once the first published archive exists, change:

```dotenv
BOOTSTRAP_EXPORT=false
```

Then enable the daily task. Keep `ALLOW_FEED_SHRINK=false`; a removed feed or a record-count
drop greater than 40% needs investigation. Use the shrink override only for a reviewed,
intentional change, and restore it afterwards.

For an independent alert while previewing, configure `reading-site` variables:

```text
STATUS_URL=https://michaelmvh.com/reading-site/status.json
MAX_SNAPSHOT_AGE_HOURS=48
STATUS_MONITOR_ENABLED=true
```

Enable GitHub Actions failure notifications and verify their delivery. GitHub-hosted
monitoring can detect stale publication even when the NAS itself is powered off.

## 7. Move the reading subdomain last

Only after the preview works, record the current tunnel/DNS settings and plan the cutover:

1. Register `reading.michaelmvh.com` as the publishing repository's Pages custom domain.
2. Change `SITE_URL` to `https://reading.michaelmvh.com/` and publish root-path output.
3. Change only the `reading` DNS record to a DNS-only CNAME targeting
   `michaelmvh.github.io`. Leave apex, `www`, and mail records alone.
4. Verify the Pages certificate, HTTPS, articles, icons, and XML at the final hostname.
5. Update RSS subscriptions and `STATUS_URL` to the final hostname.

Coordinate these steps: project-path output and root-path output are different, and DNS
and certificate provisioning can take time. A zero-downtime flip is not guaranteed.
Do not stop the old reader or tunnel during this transition.

After at least a week of successful scheduled publication and failure notifications,
retire only the old reader's public-serving components. Update the old weekly image task
separately; do not let it control the new publisher accidentally.

## Recovery and maintenance

`state/last-run.json` records prepared/deployed/unchanged/failed status without credentials.
Failed staging directories are retained for inspection. Remove only the specific failed
`state/work/run-...` directory after review; repeated failures can consume disk space.

The publisher refuses dirty checkouts, unexpected remotes, and unpushed commits. It does
not reset or discard user changes. A failed promotion keeps the old site in that attempt's
`previous-site` directory. A failed push/deploy requires inspecting the retained commit
and GitHub Actions before retrying. Git inspection can be run through a temporary DSM task
using the same image, rather than enabling SSH; ask for the specific recovery commands
before changing the checkout.

After power loss, remove `state/publisher.lock` through File Station only after confirming
no publisher container is still running. Back up published archive data and credential
files separately. Rotate expired tokens by replacing the secret file; one-off task runs
read the current files each time.

To update the publisher, select a reviewed image tag in Compose, pull it through Container
Manager, and rehearse a manual run. None of this requires updating the old HTTP container.
