# JSON-First Reading Site Migration Plan

Status: revised proposal, 2026-10-20. This plan supersedes the earlier design in which the
NAS generated and committed HTML. Updating this document does not merge implementation
PRs, publish an image, change the NAS, or move DNS.

## Goal and decisions

Keep **https://reading.michaelmvh.com**, the reader's distinct icons, individual article
pages, and RSS, while separating publication collection from website presentation.

- The Rust application on the NAS produces **structured JSON only** for the new reader.
- `reading-site` owns reader UX, TypeScript rendering/build code, styles, icons, and RSS
  generation. GitHub Actions builds the finished website from its source and the JSON.
- Keep the JSON in `reading-site/data/` initially. There are four logical components but
  only three repositories; a fourth data-only repository is not part of the initial plan.
- GitHub Pages serves the finished static site. Browsing never requires a NAS connection.
- Use one self-contained, scheduled publisher container, not host-installed publishing
  tools. SSH, host Git/Node/rsync/GitHub CLI, and a Docker socket mount are unnecessary.
- Keep ordinary settings in `publisher.env` and the two credentials in one read-only
  `secrets.env` file. Do not inject that credentials file through Compose's `env_file`.
- Strict backward compatibility is not required. Old subscriptions and bookmarks may
  need replacement; do not build legacy redirects or historical URL translation.
- Keep Cloudflare as the existing DNS provider for convenience, with a DNS-only reader
  record. Cloudflare hosting, proxying, Workers, and a tunnel are not required.

## Component ownership

| Component | Repository/location | Responsibility |
| --- | --- | --- |
| Main website | `michaelmvh/michaelmvh.github.io` | Portfolio and other main-site content; optionally link to the reader |
| Reader application | `michaelmvh/reading-site` | Reader UX, templates, styles, browser behavior, icons, static HTML/RSS build, and Pages deployment |
| Article data | `reading-site/data/` | Generated public metadata, feed membership, retained article identities, and collection freshness |
| Collector and publisher | `michaelmvh/scholarly-rss-feed`, running on the NAS | Fetch, enrich, deduplicate, retain, validate, and publish JSON snapshots |

The main website and reader do not share a build or deployment. Matching visual design
would be an explicit choice, not something inherited from using the same domain.

The Rust repository remains the source of discovery configuration, such as which authors,
journals, and categories to collect. It must not own the new website's markup or styling.
Reuse its existing provider integrations, identity matching, provenance, and caches rather
than rewriting those in TypeScript.

Reader source and generated data occupy different directories in `reading-site`. A
representative layout is:

```text
reading-site/
    src/                 # Reader templates, styles, browser code, and assets
    scripts/             # Data validation, static generation, and output checks
    schema/              # Versioned public JSON contract
    data/                # NAS-generated JSON; never hand-edited as UI source
    test/                # Contract, rendering, and browser fixtures
    .github/workflows/   # Reader build, deployment, and independent monitoring
    dist/                # Generated HTML/RSS/assets; ignored by Git
```

Only `data/` is updated by the scheduled NAS publisher. Generated HTML, XML, CSS bundles,
and browser bundles are deployed as an artifact, not committed by the NAS.

### Why not a fourth repository initially?

Keeping data alongside reader source gives one straightforward workflow: pushes affecting
either data or UX can build the site from a single checked-out revision. There is no
cross-repository dispatch credential or polling mechanism to coordinate.

This is a logical boundary, not a strong permission boundary. A GitHub token with Contents
write permission applies to the repository, not just `data/`. The publisher must stage
only the allowlisted data paths, but that safeguard does not make its token path-scoped.

If independent data history, multiple consumers, or stronger credential isolation becomes
important, move JSON to a dedicated data repository. Its NAS credential could then have
no write access to reader code. That alternative requires an explicit reader-build trigger
or polling arrangement and reproducible checkout of the selected data revision.

## Communication and build flow

```text
OpenAlex / bioRxiv / arXiv / curated collections
                        |
                        v
NAS publisher container
    Rust: fetch -> enrich -> deduplicate -> export JSON
    Publisher: validate contract -> commit only data/ -> push
                        |
                        v
reading-site repository: reader source + data/
                        |
                        v
GitHub Actions: validate JSON -> build HTML and RSS -> validate output
                        |
                        v
GitHub Pages: reading.michaelmvh.com
                        ^
                        |
              Browsers and RSS clients
```

There is no runtime API between the NAS and the public reader. JSON snapshots are their
communication contract. The browser does not contact provider APIs or the NAS.

There are two independent update paths:

| Change | Required work |
| --- | --- |
| New publication data | NAS commits JSON; reader Actions rebuilds and deploys |
| Reader layout, styling, or browser behavior | Push reader source; rebuild using the existing JSON, without contacting the NAS or providers |
| Collection logic or discovery configuration | Build/pull a reviewed collector image, then run collection |
| Main portfolio content | Existing main-site workflow only |

UI-only updates must remain possible while the NAS is offline. GitHub Pages continues
serving the last successful deployment when collection or a subsequent reader build fails.

## Versioned JSON contract

Keep one authoritative, versioned public schema in the reader repository. The collector
declares the schema version it emits; collector fixtures and integration checks must prove
that the reader accepts that output. The publisher validates against the supported contract
before committing, and the reader build validates again before rendering.

The contract must describe at least:

| Data | Purpose |
| --- | --- |
| Schema version and snapshot identity | Detect incompatible or mixed snapshots |
| Stable article IDs and publication/version identifiers | Retain identity through enrichment and produce stable URLs/RSS GUIDs |
| Titles, authors, venues, permitted abstracts, and original/PDF links | Provider-neutral content for both HTML and RSS |
| Publication dates and their precision | Avoid misrepresenting partial dates |
| Collection-added dates and provenance | Keep collection discovery separate from publication dates |
| Discovery sources and categories | Preserve all sources after deduplication |
| Named feeds, membership, tracked-author associations, and defaults | Preserve configured reader and RSS behavior without browser-side provider matching |
| Snapshot generation and successful source-check times | Measure collection freshness independently from website deployment |

Prefer canonical article records referenced by feed membership, rather than inconsistent
copies of the same publication. Preserve feed-specific tracked-author associations
separately from shared publication metadata.

JSON must not contain HTML templates, CSS classes, secrets, private provider cache dumps,
or NAS filesystem paths. Treat imported metadata as untrusted: validate identifiers,
dates, and URLs, and escape text when rendering HTML or XML.

Settle the exact file layout and any sharding after measuring a full export. Use deterministic
ordering and stable serialization so routine collection does not rewrite unrelated records.
Keep files within GitHub's per-file limits; do not put an indefinitely growing archive into
one file without a size gate. Commit all files in a snapshot together, and reject incomplete
snapshots or broken feed references.

### Retained articles and freshness

Feed retention and article retention are different. A paper leaving the recent feed should
not automatically remove its stored metadata or generated article page. Preserve the
published identity registry in retained data, back it up, and support recovery from the
committed JSON after NAS disk loss.

The reader must be able to regenerate retained article pages from JSON alone. Do not rely
on keeping old HTML around, and do not backfill aliases for obsolete NAS URLs.

Distinguish **data collection time** from **site build/deployment time**. A UI-only rebuild
must leave the data timestamp unchanged. The displayed freshness indicator and staleness
monitor must use the NAS data timestamp, not the latest Actions deployment time.

Publish metadata, appropriately reusable abstracts, attribution, and links. Do not mirror
PDFs, full papers, or upstream collection Markdown. Review source redistribution terms.

## Reader presentation and RSS ownership

The reader repository uses strict TypeScript for its build and browser code, with runtime
validation at the JSON boundary. Preserve the current lightweight, framework-free reader
unless a later UX requirement justifies a framework.

Reuse the warm off-white background, serif headings, green accents, responsive reading
column, author highlighting, SVG favicon, and Apple touch icon. This migration is not
a portfolio redesign.

GitHub Actions pre-renders complete listings and individual article pages. Essential
content, navigation, abstracts, and subscription links work without JavaScript. Browser
JavaScript enhances date/author/source filtering and navigation over already-collected
records. It may use embedded metadata or same-origin static JSON according to measured
payload needs; it must not require a live API.

Preserve default, all, custom, and explicitly empty author selections. Optional authors
remain available and remain in the complete RSS feed. Preserve selection state in URLs,
including article navigation and back/forward behavior.

Only advertise sources and categories that collection has actually included. Additional
coverage is a collector configuration change, not something a browser checkbox fetches.
Keep native-source windows bounded and avoid unfiltered category expansion or repeated
full-history downloads. Source membership survives deduplication and supports OR selection.

The reader build generates **both HTML and RSS from the same validated JSON snapshot**:

- Publish each named feed and a bounded set of explicitly configured source presets.
- Use stable item GUIDs and absolute links. Keep publication dates distinct from
  collection-added dates.
- Do not turn browser-only author/date filters into RSS filters. Label available RSS
  subscriptions accurately when a browser selection has no equivalent published preset.
- Include RSS autodiscovery tags and use real XML file URLs.
- Validate actual hosted RSS behavior with TRMNL and another RSS client before cutover.

The public site URL and deployment base path belong to the reader build, not the NAS
exporter or article metadata. The same snapshot must support a project-path preview and
the final subdomain without collecting or publishing the data again.

The new static publishing path does not ask Rust to render reader HTML or RSS. The existing
Rust HTTP server may retain those features temporarily as the current service and rollback
path; it is not the source of truth for the new reader's presentation.

## Independent hosting and Cloudflare's role

| Public address | Pages repository | Purpose |
| --- | --- | --- |
| `https://michaelmvh.com` | `michaelmvh.github.io` | Existing main website |
| `https://reading.michaelmvh.com` | `reading-site` | Independent reader, article pages, assets, and RSS |

GitHub supports a user site plus project sites in separate repositories, with a custom
subdomain assigned to an individual project. No iframe, reverse proxy, shared deployment,
or NAS web server is needed to join these sites. The portfolio can simply link to the reader.

Before assigning the reader's custom domain, preview the project site at
`https://michaelmvh.com/reading-site/`, inherited from the existing user site's domain.
The reader builder must support that base path and the final subdomain root. Once a custom
domain is assigned, it becomes canonical; do not assume the preview address remains an
independent site.

Cloudflare's roles are distinct:

| Service | Role after migration |
| --- | --- |
| DNS | Keep Cloudflare as the current DNS provider; another provider could perform the same function |
| HTTP proxy/CDN | Not required; use DNS-only routing for the reader |
| Tunnel | Not required for the new reader; retire this reader's tunnel only after successful cutover |
| Cloudflare Pages/Workers | Not part of this design |

DNS-only means Cloudflare answers DNS queries but does not proxy the website's HTTP/HTTPS
traffic. GitHub Pages hosts the reader and provides its HTTPS certificate.

Register and verify the reader custom domain in `reading-site` Pages settings before
changing its DNS record:

```text
Type:   CNAME
Name:   reading
Target: michaelmvh.github.io
Proxy:  DNS only
```

The target excludes the repository name. GitHub selects the deployment using the requested
hostname and the repository's custom-domain setting. An Actions deployment does not rely
on a generated `CNAME` file to configure the domain.

Leave apex, `www`, mail records, and unrelated tunnels alone. Allow for DNS propagation and
certificate provisioning; do not promise a zero-downtime flip. Changing DNS providers is
outside this migration and is unnecessary.

### Public URL contract and clean cutover

| Path on the reading subdomain | Output |
| --- | --- |
| `/` | Default reader |
| `/feed.xml` | Default RSS feed |
| `/feeds/<feed-name>/` | Named reader |
| `/feeds/<feed-name>/feed.xml` | Named RSS feed |
| `/article/<stable-id>/` | Individual article preview |
| `/favicon.svg`, `/apple-touch-icon.png` | Reader-specific icons |
| `/status.json` | Public-safe data freshness and separate build information |
| `/404.html` | Helpful missing-page response |

Write directory indexes so deep links and reloads work without an SPA fallback. Copy only
the static JSON needed by the browser/status endpoint into the deployed artifact; do not
force the entire retained archive into every initial page load.

Old `?rss`, `?feed=...`, arbitrary discovery queries, and historical article paths are not
compatibility requirements. Update subscriptions and bookmarks explicitly. A one-time
RSS reimport is acceptable; identity churn on later routine updates is not.

## NAS container and publication safety

Keep the self-contained, one-shot publisher image. Rust handles collection and JSON export;
Node may remain for orchestration and JSON contract validation, and Git transports data.
HTML/RSS building moves to GitHub Actions, not to another process on the NAS.

DSM File Station, Container Manager, and Task Scheduler are sufficient. No SSH service or
host publishing-tool installation is required. Mount isolated persistent state and
credentials; expose no ports and mount no Docker socket.

Use these configuration boundaries:

```text
publisher.env   # Repository, collection/publishing mode, contact, and commit identity
secrets.env     # PUBLISH_GITHUB_TOKEN and GSRF_OPENALEX_API_KEY
state/          # Private caches, checkout, staging, retained state, and run summaries
```

Read `secrets.env` as a read-only file containing `KEY=VALUE` entries, not as a Compose
`env_file`. Keep it private and excluded from Git and image build contexts. The GitHub
fine-grained token needs Contents read/write and Actions read on `reading-site`; it does
not need workflow-editing permission. Mounted credentials remain within the container's
trust boundary, so use only reviewed code.

A verified OpenAlex key is required for cursor paging beyond the first page. Never ask
the user to put its value in a PR, chat, or public configuration. Do not infer missing
credentials or silently truncate an anonymous export.

Start in prepare-only mode: export and validate JSON locally without commits or pushes.
Publish mode may update **only `data/`**, never reader source, workflows, `site/`, or `dist/`.
Use a fresh staging directory and one commit per complete snapshot. Preserve dirty or
unpushed work for inspection instead of resetting it.

Reconcile concurrent reader-source changes before pushing and revalidate against the
applicable schema. Fail explicitly on conflicts or incompatible contracts; never force-push
or overwrite UI changes. The publisher must distinguish a successful push from a successful
reader build/deployment and report failed or skipped deployment clearly.

## Scheduling, failures, and operational limits

Propose daily collection, with a manual run available. Keep it separate from the existing
weekly **Update Scholar RSS** image-update task. UI updates should need no NAS image update.

Use a run lock, bounded requests, explicit exit codes, and logs. Required discovery-refresh
failures abort data publication; incremental caches and successful conditional requests
remain useful. Optional enrichment can fail only with an explicit report and usable base
records. Reject empty/truncated required feeds and unexpected large coverage drops.

The reader workflow triggers for relevant source and data changes and supports manual runs.
It validates JSON, builds from one checked-out revision, checks generated HTML/RSS/assets,
and deploys only `dist/` as a Pages artifact. Serialize deployments so older snapshots cannot
replace newer ones. The build needs no NAS credentials or provider API keys.

Alert on collection, push, build, and deployment failures. Retain independent GitHub-hosted
monitoring, initially with a 48-hour data-staleness threshold. UI-only deployments must not
reset that timer. Configure and verify an operator notification channel.

Measure full-snapshot size, file counts, Git growth, build time, and mobile loading/filtering
performance before rollout. Preserve substantial headroom under GitHub's repository,
per-file, bandwidth, and Pages limits; the documented published-site limit is 1 GB. Retain
a conservative deployment size gate and set data-file/sharding thresholds from measurements.
Do not automatically rewrite Git history to control growth.

## Reconcile the existing draft PRs before deployment

The existing drafts were largely built around NAS-generated HTML. Their passing checks do
not establish that this revised architecture is implemented. They must be refactored and
their operator documentation reconciled before being treated as deployment instructions.

| Draft | Required disposition |
| --- | --- |
| `scholarly-rss-feed` #2: static export | Reuse collection, identities, and retention; change the new publishing output to JSON only |
| `scholarly-rss-feed` #3: static reader | Move reader UX/rendering ownership to `reading-site`; port browser behavior and its tests |
| `scholarly-rss-feed` #4: host orchestration | Superseded by the container-only approach; do not install host tools |
| `scholarly-rss-feed` #5: freshness policy | Retain required-refresh behavior for data publication |
| `scholarly-rss-feed` #6: OpenAlex authentication | Retain private key handling and pagination support |
| `scholarly-rss-feed` #7: publisher container | Retain container/credential design; publish only `data/` and remove reader URL/build configuration from NAS export |
| `reading-site` #1: publishing infrastructure | Add reader source and a JSON-to-HTML/RSS build; replace prebuilt-`site/` deployment with a generated `dist/` artifact |
| `reading-site` #2: monitoring | Monitor the NAS data timestamp independently of reader build/deployment time |

Do not merge the old implementation stack as though it already satisfies this plan. Preserve
useful work, but review the resulting dependency order and contract integration again.

## Implementation phases and acceptance gates

### Phase 1: Contract and baseline

Define component ownership, schema/version handling, retained identities, feed membership,
date semantics, and source freshness. Capture representative real-data fixtures and measure
initial volume. Agree on file layout, useful RSS presets, and collected source coverage.

Acceptance: a representative JSON snapshot is sufficient to build listings, article pages,
and RSS without a provider/NAS call or embedded HTML.

### Phase 2: Rust JSON producer

Refactor export mode around the public data contract. Preserve provider integrations,
deduplication, author matching, refresh policy, and persistent state. Keep the old HTTP
service independently available during migration.

Acceptance: deterministic JSON, stable identities, retained articles, explicit source
failures, and no private fields or presentation markup in the exported contract.

### Phase 3: Reader-owned static build

Move reader presentation into `reading-site`. Implement runtime validation, TypeScript
rendering, responsive styles, icons, browser filters, RSS generation, and artifact deployment.

Acceptance: JSON fixtures produce valid HTML/XML; essential content works without JavaScript;
author/source/date behavior and deep links match the agreed semantics. A UI-only change
deploys successfully while the NAS is offline and leaves the data timestamp unchanged.

### Phase 4: Container and preview integration

Adapt the container publisher to commit only `data/`. Rehearse prepare-only and publishing
modes, credentials, first-run recovery, overlapping runs, incompatible schemas, source
failures, concurrent UI changes, and failed builds/deployments. Deploy at the project-site
preview address without changing the production reader's DNS.

Acceptance: only generated JSON changes in the publishing commit; Actions creates the site;
the preview works without NAS access. Complete full-feed collection with a privately
configured OpenAlex key before declaring the production dataset ready.

### Phase 5: Domain and subscriptions

Register the custom domain, publish output for the subdomain root, change only the reading
DNS record, verify HTTPS, and update subscriptions/bookmarks and monitoring URLs.

Acceptance: final-hostname listings, articles, icons, and RSS work; TRMNL fetches XML; the
portfolio and mail configuration remain unchanged. No legacy compatibility layer is needed.

### Phase 6: Observe and retire old serving

Observe at least one week of scheduled data updates and useful failure notifications.
Keep the old NAS service, image, configuration, and caches available during this interval.
Then retire only this reader's public-serving container/tunnel and revise image-update tasks.

Acceptance: public reads require no NAS connectivity, independent staleness monitoring works,
and UI and data updates can proceed independently.

## Rollback and remaining decisions

For bad data, restore a known-good JSON revision and rebuild. For a reader regression, restore
reader source without discarding newer valid data, provided the schema remains compatible.
Retain artifact/data revision identifiers so rollback uses a compatible pair.

For domain-cutover problems, restore the old tunnel/DNS mapping and manually restore old
RSS URLs in active clients. Temporary interruption and URL changes are acceptable; do not
add compatibility routes to the old service just for rollback. Keep private caches intact.

Decisions still needed during implementation are the final JSON layout/schema details,
measured size/retention budgets, optional-source coverage and RSS presets, daily run time,
and notification/backup procedures. The initial repository boundary, JSON-only NAS output,
reader-owned build, separate subdomain, credential-file format, and DNS-only hosting approach
are the baseline of this revised plan.

## References

- [Current service and deployment documentation](../README.md)
- [Native preprint source plan](native-preprint-sources-plan.md)
- [GitHub Pages user and project sites](https://docs.github.com/en/pages/getting-started-with-github-pages/what-is-github-pages#types-of-github-pages-sites)
- [Custom domains across repositories](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/about-custom-domains-and-github-pages#using-a-custom-domain-across-multiple-repositories)
- [Configuring a Pages subdomain](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site#configuring-a-subdomain)
- [Custom Pages workflows](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages)
- [GitHub Pages limits](https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits)
- [Cloudflare DNS-only versus proxied records](https://developers.cloudflare.com/dns/proxy-status/)
- [OpenAlex authentication and cursor paging](https://help.openalex.org/api/authentication/)
