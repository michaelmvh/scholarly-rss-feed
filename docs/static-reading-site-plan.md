# Static Reading Site Migration Plan

Status: proposed; this document does not change the running service, DNS, or NAS tasks.

## Goal and preferred architecture

Keep the reader at **https://reading.michaelmvh.com**, with its own existing icons,
individual article pages, and subscribable RSS feeds. Move public serving from the NAS to
GitHub Pages while retaining the NAS for scheduled discovery and generation.

```text
scholarly-rss-feed source repository
    -> GitHub Actions publishes the application image to GHCR
    -> NAS runs the image on a schedule with persistent discovery caches
    -> application exports a validated static site
    -> NAS pushes changed output to a separate publishing repository
    -> that repository's GitHub Actions deploys a Pages artifact
    -> reading.michaelmvh.com serves the last successful deployment
```

The NAS no longer needs to be reachable by readers. A NAS outage delays new publications
but does not take down previously published pages or RSS. Freshness changes from
request-driven generation to scheduled updates plus deployment time.

Use a separate publishing repository, provisionally named `reading-site`. Keep the Rust
application, templates, browser code, icons, feed configuration, and exporter tests in
`scholarly-rss-feed`. The publishing repository contains generated public output and a small
deployment workflow; it is not a second implementation of the reader. The portfolio
repository does not need to change.

## Requirements to preserve

| Requirement | Planned behavior | Main challenge |
| --- | --- | --- |
| Separate subdomain | Continue using `reading.michaelmvh.com` over HTTPS | DNS, Pages domain registration, certificate provisioning, and rollback must be coordinated |
| Unique icons | Keep the existing SVG favicon and 180x180 PNG Apple touch icon | Safari and Home Screen bookmarks may retain cached icons |
| RSS | Publish real XML files with stable item identities and absolute links | Existing `?rss` URLs cannot select XML on Pages alone |
| Individual article pages | Pre-render one readable HTML page per article, with stable paths | Preserve aliases when enrichment or deduplication changes a record's preferred ID |
| Reader filters | Filter exported records without contacting the NAS | Current controls submit to a server; browser filtering needs implementation |
| Optional sources | Collect supported categories in scheduled jobs, but leave them unselected by default | Selecting a source can no longer initiate discovery on demand |
| Optional authors | Preserve default, custom, empty, and all-author selection semantics | Reader defaults must not silently remove optional authors from RSS |
| Availability | Serve the last successful complete deployment during refresh failures | Monitor data freshness separately from website uptime |

Essential listings, navigation, abstracts, and RSS links remain usable without JavaScript.
Interactive filtering can be progressive enhancement; a no-JavaScript page should expose
named feeds/presets and explain that custom filtering requires JavaScript, not submit a
form that appears to work but does nothing.

## Existing implementation to reuse

- `src/main.rs`: feed resolution, provider orchestration, `build_feed`, and RSS conversion.
- `src/works.rs` and `src/provenance.rs`: identity matching, deduplication, and source provenance.
- `src/snapshot_store.rs`, `src/curated.rs`, and `src/discovery/`: persistent source snapshots
  and incremental discovery.
- `src/reader/`: reader models, filtering semantics, HTML escaping, and article rendering.
- `src/assets/`: reader styling, browser behavior, and reader-specific icon assets.
- `feeds.toml`: named feed definitions and author defaults.
- `NAS/docker-compose.yml` and the README deployment instructions: current NAS operation.

Add a run-once export mode to the current application rather than scraping its HTTP output
or rebuilding the discovery pipeline in another language. Extract shared generation and
rendering code only as needed so the HTTP mode and exporter use the same rules.

The current reader script submits forms; it does not implement standalone dataset filtering.
The current server also generates articles and source-specific feeds on demand. Copying
today's HTML to Pages is therefore insufficient.

## Published output and URL contract

Proposed paths, all under `https://reading.michaelmvh.com`:

| Path | Purpose |
| --- | --- |
| `/` | Default reader, currently the `bioml` feed |
| `/feed.xml` | Stable default RSS subscription |
| `/feeds/<feed-name>/` | Reader for a configured named feed |
| `/feeds/<feed-name>/feed.xml` | RSS for that named feed |
| `/article/<stable-id>/` | Full static article preview |
| `/data/<feed-name>.json` | Versioned public metadata needed by reader filters |
| `/favicon.svg` | Existing reader SVG icon |
| `/apple-touch-icon.png` | Existing reader Home Screen icon |
| `/reader.css`, `/reader.js` | Shared first-party reader assets |
| `/status.json` | Public-safe data freshness and generation metadata |
| `/404.html` | Clear missing-page response with navigation to the reader |

Write directory indexes for reader and article paths so direct navigation and browser
refresh work without an SPA fallback. Preserve the current hex-encoded article identifiers
where possible and test Pages' redirect from old slashless article paths to directory URLs.
Support a configurable base path for a pre-cutover project-site preview; production stays
at the subdomain root.

Keep identifiers independent of titles, rendering order, and generation timestamps. Maintain
a durable article/alias registry so a preprint later merged with a published work retains its
previously published URLs. Bootstrap aliases from known current records; do not promise
recovery of every historical URL if no record of that identifier remains.

Feed retention and article-page retention are different: papers can leave the rolling feed
without deleting their published article pages. Persist the article registry and retained
metadata on the NAS, back them up, and support restoring published records from the publishing
repository after disk loss. Do not retain only old HTML that can never receive template fixes.
Track archive growth and establish a retention review before approaching hosting limits.

Expose only public article metadata and appropriately reusable abstracts, provenance, and
publisher/PDF links. Do not mirror full papers, provider responses, upstream collection
Markdown, private caches, contact configuration, or credentials. Preserve attribution and
review source terms for static redistribution. Escape external text in both HTML and XML.

## Reader and source behavior

Generate complete HTML for the default view of each named feed. Export enough normalized
metadata for browser filtering, preserving the current date precision, collection-added
dates, tracked-author identity, optional status, and discovery provenance.

Preserve the current author rules: default selection hides optional-only tracked-author
papers, all-author mode includes untracked papers, and an explicitly empty custom selection
shows no papers. Preserve selection state in URLs, including navigation to and from articles.
Do not introduce browser-local preferences that override a shared URL.

Collect the configured core/curated sources and supported optional native categories before
publication, using bounded windows and existing incremental caches. Do not expand to
unbounded arXiv categories or backfill all history on every run. Export per-source membership
through deduplication so source selection remains meaningful for papers discovered more
than once. Test selection-dependent metadata as well as membership: merging all sources
first can change the representative record compared with today's selected-source pipeline.

Start with the existing supported category set, subject to measured export size and run
time. If it cannot fit the agreed budgets, narrow the set explicitly before cutover rather
than silently dropping an advertised source. Load optional/archive datasets only when needed
if a single payload becomes too large.

Arbitrary author, journal, ORCID, topic, or date-window queries are not part of the initial
static service. Adding a discovery input requires a configuration change and a scheduled
export. Browser filters operate only on the exported data.

## RSS preservation and legacy URLs

RSS itself is straightforward on static hosting: generate valid XML and link to the same
canonical feed files from reader buttons and HTML RSS autodiscovery tags.

- Preserve item GUIDs and their permalink semantics; a host migration must not make every
  existing paper appear new. Keep publication dates distinct from collection-added dates.
- Preserve the current default RSS content, including optional authors. Reader-only date and
  author filters must not become RSS filters.
- Generate each named feed and a bounded set of explicitly configured source presets.
  Do not export every possible combination of source checkboxes.
- When a browser source selection has no matching published RSS preset, label the available
  subscription accurately instead of claiming it matches the current selection.
- Use absolute public URLs in RSS, a suitable XML content type, and an update/TTL policy
  consistent with the export cadence. Validate the actual hosted response with TRMNL and
  a general RSS reader.
- Check any required CORS behavior against the hosted XML. Pages does not provide arbitrary
  per-file response-header configuration; use same-origin data for the reader itself.

### Why existing subscriptions need special treatment

Today, `/?rss` and `/?feed=protein-design-archive&rss` return XML. Pages serves the same
root HTML file regardless of those query parameters. Browser JavaScript, an HTML redirect,
or an SPA-style 404 cannot repair this for RSS clients.

Two supported migration choices must be decided before DNS cutover:

| Choice | Benefits | Costs and limitations |
| --- | --- | --- |
| Pure Pages, new XML URLs | Simplest hosting; no public application or edge logic | Update TRMNL and other subscriptions before switching DNS; unknown old subscriptions will break |
| Pages plus Cloudflare legacy redirects | Can retain known old RSS subscription URLs without keeping the NAS public | Keep Cloudflare proxying for this hostname; maintain and test query-aware HTTP redirects and TLS/cache configuration |

Prefer XML file URLs for all new subscriptions. If preserving existing subscriptions is
required, retain a small Cloudflare compatibility layer for known default/named-feed/preset
URLs. Confirm the account's available rule features before choosing Redirect Rules or a
Worker; do not assume arbitrary query parsing or combinations fit a free rule quota.

Match query parameters independent of order, including bare `rss`, and remove legacy
parameters from the redirect destination. Test duplicate/encoded parameters and unknown feed
names. Never redirect an unsupported custom feed to an unrelated default feed as if it were
equivalent. Inventory actively used custom subscriptions and either publish matching presets
or explicitly migrate them. Use temporary HTTP redirects during rehearsal before making
them permanent; verify that the actual RSS clients follow them.

Browser-only legacy URLs such as `/?feed=protein-design-archive` can be handled with a small
client-side navigation shim, or with edge redirects if that layer is retained. The root HTML
must still offer ordinary named-feed navigation without JavaScript. Preserve supported view
parameters; report unsupported discovery parameters instead of silently ignoring them.

## Subdomain, HTTPS, and icons

Register and verify `reading.michaelmvh.com` for the new publishing repository's Pages site
before directing DNS to it. Its custom domain is separate from the portfolio's apex and
`www` configuration. Leave the portfolio and mail records unchanged.

The eventual DNS target for `reading` is `michaelmvh.github.io`, not a repository path and
not the NAS tunnel target. With an Actions-based Pages deployment, the custom domain is
configured in repository settings; a generated `CNAME` file is not a substitute.

For pure Pages, use a DNS-only CNAME and enable HTTPS once GitHub provisions the certificate.
Allow for DNS propagation and certificate delays; do not promise a zero-downtime flip.
If retaining Cloudflare redirect rules, they require a proxied record. Verify the Pages
origin certificate and strict HTTPS to the origin, and test both origin and edge behavior.
Exclude RSS from browser challenges. Validate cache behavior so a legacy HTML response or
old feed is not retained after cutover. Do not disable TLS verification as a workaround.

Keep `/favicon.svg` and `/apple-touch-icon.png` on the reading subdomain and include their
existing link tags on every reader and article page. Reuse the actual reader assets rather
than the portfolio's icons. No service worker or PWA rewrite is required. Test Safari,
desktop browser tabs, and an iOS Home Screen bookmark; icon caches may require recreating a
bookmark if artwork or URLs change. Keeping the same host and icon paths minimizes churn.

## Scheduling, publication, and failure handling

Propose one daily NAS export initially, with manual refresh available. Keep this separate
from the existing weekly **Update Scholar RSS** Task Scheduler job, which updates containers
rather than generating a scheduled public snapshot.

The export task must use a persistent cache volume, a run lock, bounded upstream requests,
explicit exit codes, and logs. Track the application image/version used. Publish only after
all required feeds and assets pass validation.

Required discovery failures abort publication unless an explicitly approved policy permits
a last-known-good source snapshot with visible source freshness. Optional enrichment may
remain best-effort only when usable base records survive and the failure is reported.
Do not publish a successful-looking empty or unexpectedly truncated feed after an outage.
Establish coverage checks and record-count review thresholds from the initial baseline,
with an explicit override for intentional configuration changes.

Generate into a fresh staging directory. The publishing task copies only an allowlisted
output directory into the publishing checkout, leaving its workflow and repository metadata
untouched. Push a complete snapshot in one commit and deploy it as one Pages artifact.
Never upload files incrementally to the live site.

Use a repository-scoped write credential stored privately on the NAS, such as a dedicated
SSH deploy key. Bootstrap the deployment workflow separately so routine data publication
does not require workflow-editing privileges. Protect credentials, avoid logging them,
and document rotation/revocation. Confirm the credential and branch rules actually allow
the intended push/deployment flow.

Use deterministic ordering and content-aware timestamps to avoid rewriting every article or
RSS item daily. Push only public content or freshness changes. It is acceptable for a small
freshness manifest to change after a successful no-new-paper refresh; do not rewrite all
pages just to record that check.

The deployment workflow validates only the generated output, uploads that directory using
the supported Pages artifact action, and deploys through the `github-pages` environment with
minimal permissions. Serialize publication/deployment so an older snapshot cannot replace
a newer one. Confirm a successful push is followed by a successful deployment; they are
separate failure points.

Show last successful source refresh times separately from site generation/deployment time.
Alert on failed export, failed push, failed deployment, and excessive staleness. A proposed
initial stale threshold is 48 hours for daily collection. Notifications should use an
explicitly configured operator channel rather than silently relying on task logs.

## Size, maintenance, and hosting limits

Metadata and HTML should be much smaller than mirroring PDFs, but optional source datasets,
retained article pages, and Git history will grow. Measure initial output bytes, file counts,
mobile filter performance, daily changed bytes, and expected annual growth before rollout.

GitHub currently documents a 1 GB published-site limit, a recommended 1 GB source repository
limit, a soft 100 GB/month bandwidth limit, and a 10-minute Pages deployment timeout.
Custom Actions deployments avoid the standard soft build-frequency limit, but not the site
size limit. Keep substantial headroom; review limits again during implementation.

Do not rewrite publishing-repository history automatically to manage growth. If measurements
make committed generated output impractical, separately evaluate publishing a Pages artifact
without retaining every generated revision in Git, or using another static host.

Public hosting remains public even if a paid plan permits a private source repository.
Choose repository visibility and confirm account eligibility before creation; assume no
private reading data can be stored in the published output.

## Implementation phases and acceptance gates

### Phase 1: Baseline and explicit decisions

Inventory live feed URLs, custom RSS subscriptions, article paths, current icon assets, feed
sizes, source freshness, and default/filter behavior. Capture representative fixtures,
including optional-author and merged-version cases. Confirm publishing repository visibility,
RSS compatibility choice, category coverage, refresh cadence, and retention budget.

Acceptance: the URL compatibility matrix and intentionally unsupported behaviors are agreed;
there is no assumption that query-based RSS will work on Pages by itself.

### Phase 2: Run-once export and durable identities

Add the exporter, shared rendering support, static URL generation, article/alias registry,
public-data schema, source freshness manifest, and complete-output validation. Preserve the
HTTP service as the production and rollback path.

Acceptance: fixture exports are deterministic aside from intentional freshness metadata;
XML parses; required files and links exist; known old article IDs still resolve; an upstream
failure cannot replace good output with missing feeds. Rebuilding from retained metadata
recreates old article pages after they leave active feed windows.

### Phase 3: Static reader and RSS parity

Implement browser filtering and URL state, named feeds, source presets, icon references,
article navigation, and RSS autodiscovery. Keep discovery logic out of the browser.

Acceptance: default/custom/all/empty author cases, optional authors in RSS, source OR
selection, date semantics, merged provenance, direct article navigation, and back navigation
match the agreed behavior. Essential content works without JavaScript; keyboard controls
and responsive layouts remain usable. Filtering and page loads make no NAS/provider requests.

### Phase 4: Publishing repository and NAS rehearsal

After approval, create the publishing repository, configure Pages Actions and credentials,
and add the independent NAS export task. Rehearse several update cycles on a preview URL
without changing production DNS.

Acceptance: no-change, new-paper, enrichment, source failure, concurrent-run, push failure,
and deployment failure cases behave as designed. Inspect emitted HTML/XML/JSON for private
data. Confirm that stopping the NAS leaves the preview reader and RSS available.

### Phase 5: Domain and subscription cutover

Register the custom domain, record the old DNS/tunnel settings, and establish the selected
legacy URL strategy. Migrate known RSS clients or stage and verify redirects. Switch only
the reading hostname, complete HTTPS checks, and verify the production reader, article
deep links, feed responses, icons, and freshness display.

Acceptance: TRMNL and a general RSS client fetch actual XML without duplicate-item floods;
supported legacy URLs behave as documented; article and icon URLs work on the custom domain;
the portfolio and mail configuration remain unchanged.

### Phase 6: Observation and retirement

Observe at least one week of scheduled publication and failure notifications. Keep the
previous NAS service, configuration, and required caches available during this window.
Only then retire this service's public tunnel and continuous web-server container, and
update deployment documentation and the weekly image-update procedure for run-once use.

Acceptance: public reads require no NAS access, publication and staleness monitoring work,
and the last-good static deployment remains usable during a controlled NAS outage.

## Rollback

Before cutover, retain the previous application image reference, Compose/environment files,
DNS/tunnel mapping, and known-working RSS URLs. Do not delete persistent discovery caches.

For bad generated content, redeploy a known-good publishing revision. For cutover problems,
restore the reading hostname's previous tunnel mapping and disable conflicting edge rules;
DNS caches may delay recovery. Keep redirects reversible during observation. Because some
clients may already use `/feed.xml` and named static paths, prepare compatible routes in the
fallback service before cutover, or retain a known-good static origin for those URLs.
Rollback must account for both old and newly issued subscription URLs.

## Decisions required before implementation

- Final publishing repository name, visibility, and publishing credential mechanism.
- Pure Pages with migrated subscriptions, or Pages plus legacy Cloudflare redirects.
- Which custom/source-specific RSS subscriptions need published presets.
- Optional-source coverage, daily schedule, notification channel, and stale-data policy.
- Article retention/growth budget and backup/restore procedure.

These decisions do not block reviewing this plan. Creating the publishing repository,
changing DNS, adding NAS jobs, and migrating subscriptions require separate implementation
approval.

## References

- [Current deployment and feed behavior](../README.md)
- [Native preprint source plan](native-preprint-sources-plan.md)
- [GitHub Pages custom domains](https://docs.github.com/en/pages/configuring-a-custom-domain-for-your-github-pages-site/managing-a-custom-domain-for-your-github-pages-site)
- [GitHub Pages custom workflows](https://docs.github.com/en/pages/getting-started-with-github-pages/using-custom-workflows-with-github-pages)
- [GitHub Pages limits](https://docs.github.com/en/pages/getting-started-with-github-pages/github-pages-limits)
- [Cloudflare redirects and proxy requirements](https://developers.cloudflare.com/rules/url-forwarding/)
