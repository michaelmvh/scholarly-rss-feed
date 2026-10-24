# Scholarly RSS feed generator

Generates a single RSS feed of recent scientific publications for one or more authors
**and/or journals**, sorted newest-first. Data is parsed from
[OpenAlex](https://openalex.org), a free, open catalog of scholarly works. Basic queries work
without a key, but cursor paging beyond the first page requires a verified free API key.
The project's original Google Scholar scraper is retained as an archived fallback
provider that can be enabled with one environment variable.

Feeds can be defined in a config file so a feed URL never has to change. Each URL provides a
lightweight browser reader by default and raw RSS through the `rss` query parameter, making it
convenient to use both on the web and in a display such as a
[TRMNL](https://usetrmnl.com) via its RSS plugin.

This project was originally based on
[Julien-cpsn/google-scholar-rss-feed](https://github.com/Julien-cpsn/google-scholar-rss-feed)
and has since been redesigned around OpenAlex. Its Google Scholar scraping behavior is
preserved and hardened in [`src/google_scholar.rs`](./src/google_scholar.rs). The original
copyright and MIT license notice are retained in [`LICENSE`](./LICENSE).

## Deployment overview

The repository and published container are available at:

- **Source:** <https://github.com/Michaelmvh/scholarly-rss-feed>
- **Container:** <https://github.com/Michaelmvh/scholarly-rss-feed/pkgs/container/scholarly-rss-feed>
- **Image:** `ghcr.io/michaelmvh/scholarly-rss-feed:latest`
- **One-shot publisher image:** `ghcr.io/michaelmvh/scholarly-rss-feed-publisher:latest`

The production update path is:

```text
Edit code or feeds.toml
        |
        v
Push to GitHub main
        |
        v
GitHub Actions builds linux/amd64 and publishes to GHCR
        |
        v
The NAS pulls the new image and recreates the container
```

The application and `feeds.toml` are both baked into the image. The NAS therefore needs only
[`NAS/docker-compose.yml`](./NAS/docker-compose.yml) and a private `.env` containing the
Cloudflare token and provider settings. It does not clone the source or compile Rust.

The image and Compose file update independently:

- **Code or feed changes:** push to GitHub, then pull the new GHCR image on the NAS.
- **Deployment changes:** upload the changed `NAS/docker-compose.yml` to the NAS and recreate
  the Container Manager project. GHCR cannot update the Compose file itself.

### Proposed static hosting

The [JSON-first reading site migration plan](docs/static-reading-site-plan.md) separates
Rust collection on the NAS from reader presentation: the NAS publishes JSON to
`reading-site/data/`, and that repository's TypeScript build generates HTML and RSS for
GitHub Pages. It preserves `reading.michaelmvh.com` and the reader's icons, with independent
portfolio/reader deployments, DNS-only routing, and no strict legacy URL compatibility.
The revised implementation is consolidated in [collector/publisher PR #2](https://github.com/michaelmvh/scholarly-rss-feed/pull/2)
and [reader PR #1](https://github.com/michaelmvh/reading-site/pull/1). Earlier implementation
drafts are superseded by those branches. Reviewed merges, image publication, and a private
key-backed NAS rehearsal are still required before cutover. The HTTP deployment instructions
later in this README describe the existing fallback service.

### Run-once JSON export

The new reader's public contract lives in
[`reading-site/schema/snapshot-v1.json`](https://github.com/michaelmvh/reading-site/blob/main/schema/snapshot-v1.json).
This repository owns collection, enrichment, deduplication, identities, and JSON export.
`reading-site` owns the new reader's TypeScript UX, HTML/RSS rendering, icons, and deployment.
The existing HTTP server remains available separately as the legacy service.

```sh
cargo run --locked -- --export /tmp/reading-data-next --config feeds.toml
```

The destination must be fresh. Subsequent exports use
`--previous /path/to/last-successful-data` to retain article identities and metadata.
The exporter writes **only JSON**, never HTML, XML, CSS, JavaScript, or icons:

```text
snapshot.json       # Schema version, data timestamp, feeds, and shard checksums/counts
articles/ab.json    # Retained canonical records, bucketed by the first two ID characters
articles/cd.json
```

The publisher commits this directory as `reading-site/data/`. Stable sharding avoids one
ever-growing file and limits unrelated Git changes. Each JSON file is limited to 50 MiB
and the complete snapshot to 500 MiB. A manifest authenticates the bytes of every shard
with SHA-256 and describes their expected record counts. It does not provide a cryptographic
signature or replace access control.

Feed membership and tracked-author associations/defaults are separate from canonical
article metadata, so two feeds can select the same paper differently. Identifiers retain
known provider/version aliases. Ambiguous identity merges fail for review rather than
silently dropping a previously published identity.

Dates retain the source value and collection-added dates remain separate. The current
provider-neutral model does not expose reliable publication-date precision, so the exporter
reports `unknown` rather than inferring precision from January 1 or the first of a month.
The contract also supports explicit day/month/year precision when producers can establish it.

The entire previous JSON snapshot is public metadata, not a private provider cache. Keep
it backed up. Empty required feeds, unsafe URLs, malformed/checksum-invalid previous data,
and required source-refresh failures stop publication. Incremental caches and successful
conditional requests remain supported. Optional enrichment may fail with an explicit log;
the HTTP service retains its existing stale-cache fallback policy.

The public site URL belongs to the reader build. `--site-url` is no longer accepted here.
Changing preview/production domains or reader styling does not require recollecting data.

For a synthetic fixture with no provider requests:

```sh
GSRF_TEST_EXPORT_DIR=/tmp/reading-data-fixture \
  cargo test --locked export_fixture -- --ignored
```

CI uploads this JSON fixture. Reader rendering/browser tests now belong to `reading-site`;
source tests retain the existing HTTP-reader coverage and publisher orchestration coverage.
Collector CI also pins a compatible reader revision and runs its build/browser checks against
the actual Rust fixture. Update that pin deliberately when changing the public contract.

For the staged NAS migration, see [the static publication runbook](NAS/STATIC-PUBLISHING.md),
[`NAS/static-compose.yml`](NAS/static-compose.yml), and
[`NAS/publisher.env.example`](NAS/publisher.env.example). A separate one-shot publisher
container includes the Rust exporter, Node.js validation runtime, and Git. It polls GitHub's
REST API instead of requiring GitHub CLI. No Git/Node/rsync installation, SSH service, or
Docker socket is needed on the NAS. The existing always-on reader remains separate.

The publisher defaults to `PUBLISH_MODE=prepare`: it generates and validates JSON without
committing, pushing, or deploying. `PUBLISH_MODE=publish` commits only `data/`, preserves
concurrent UI commits, and confirms the matching reader build/Pages deploy job. It never
commits `site/`, `dist/`, reader source, or workflow changes. `SITE_URL` must be removed from
old publisher configuration; `READER_SITE_URL` is a reader-repository Actions variable.
A dedicated state mount preserves the checkout, article history, caches, lock, and a
credential-free `last-run.json`.

GitHub and OpenAlex credentials are read from one read-only `secrets.env` file, based on
[`NAS/secrets.env.example`](NAS/secrets.env.example). It uses ordinary `KEY=VALUE` syntax,
but is mounted as a file rather than injected through Compose's `env_file`. The GitHub
token is scoped to `reading-site`, with Contents read/write and Actions read permission.
Only Git's child-process environment and GitHub API requests receive that token; the exporter
environment receives only its OpenAlex key, and npm/validation environments receive neither.
The mounted credentials file remains within the container's shared trust boundary: use only reviewed
images and publishing-repository code. Git remotes are checked before use, and tokens are not
embedded in URLs, generated files, or the image.

The Dockerfile has explicit `server` and `publisher` targets; the default remains `server`.
GitHub Actions publishes both images for `linux/amd64`. Publisher orchestration is strict
TypeScript run directly by Node.js 24. Validation installs only the reader repository's
production dependencies; UI compilation/browser tooling is not run on the NAS.
Use `npm run typecheck:publisher` and `npm test` for
local checks. CI also builds the real publisher image and runs its orchestration tests
with no network, a read-only root filesystem, and no Docker socket.

## Feed configuration

Named feeds live in [`feeds.toml`](./feeds.toml), keeping the public URL stable when authors,
journals, or topics change.

### Protein design and biomolecular models

The `protein-design` feed combines a rolling year of OpenAlex author results with four
bounded field-wide title/abstract searches, restricted to articles, preprints, and reviews
rather than dataset/software records. It covers protein, peptide, antibody, binder,
enzyme, and protein-nanomaterial design, inverse folding, generative protein models, and
enabling structure/interaction/affinity models such as Boltz. The existing `bioml` feed
keeps its selection apart from correcting Baker's retired OpenAlex ID. The Peldom-backed
`protein-design-archive` is unchanged; this live feed does not depend on Peldom updates
or refresh availability.

Tracked researchers are David Baker, Frank DiMaio, Sergey Ovchinnikov, Brian Hie,
Kevin K. Yang, Pranam Chatterjee, Nicholas F. Polizzi, Charlotte Mary Deane, Neil King,
Gaurav Bhardwaj, Green Ahn, Regina Barzilay, Tommi Jaakkola, Jeremy Wohlwend, Gabriele
Corso, Saro Passaro, Hannes Stark, Noah Getz, and Mateo Reveiz. OpenAlex IDs are explicit
in `feeds.toml`; new IDs are checked against relevant paper authorships. Baker's current
ID is used in both live feeds because his previous ID no longer resolves.

`relevance_filter = "protein_design"` applies to all candidates, including tracked-author
papers. It requires title or single-abstract-sentence evidence of design/engineering/
optimization near proteins or related molecules, protein inverse folding, generative protein
sequence/structure models, or enabling biomolecular prediction methods. An author match
or a model name mentioned alone is insufficient. Abstract-only evidence must occur in a
sentence describing the work's contribution, not just background about an existing
designed therapeutic. RNA/DNA-focused titles without protein evidence, materials binders,
general clinical AI, generic graph ML, and descriptive structural biology without this
evidence are excluded. Clear titles can qualify without abstracts. Inclusions, exclusions,
and refresh totals are logged. This lexical gate can miss unusual terminology and admit
incidental background statements;
it is not a completeness guarantee or a semantic classifier.

After the updated collector image is deployed and publishes a snapshot, the existing
reader collection menu exposes:

- <https://reading.michaelmvh.com/feeds/protein-design/>
- <https://reading.michaelmvh.com/feeds/protein-design/feed.xml>

Native preprint-category ingestion is not enabled for this feed. Papers indexed by
OpenAlex can include preprints, and existing version deduplication and canonical article
identities are reused. Field-search candidate limits and required-refresh failures stop
publication rather than silently truncating results.

### Protein nanopores and protein sequencing

The separate `protein-nanopores` feed tracks a rolling year of nanopore-based
protein/peptide sequencing, sensing, amino-acid discrimination, and motor-controlled
translocation, including solid-state pores when used for protein measurements. It combines
field-wide OpenAlex title/abstract searches with verified author IDs for Jeff Nivala,
Cees Dekker, Henry Brinkerhoff, Meni Wanunu, Giovanni Maglia, Chunzhe Lu, Shuo Huang,
and Aleksei Aksimentiev. Native bioRxiv discovery covers `biophysics` and `bioinformatics`,
including preprints not yet indexed by OpenAlex. All candidates, including author and
native-preprint matches, pass the same relevance gate. Existing feeds and subscriptions
are unchanged.
After the updated collector is deployed and publishes a snapshot, the static reader and RSS
are available at:

- <https://reading.michaelmvh.com/feeds/protein-nanopores/>
- <https://reading.michaelmvh.com/feeds/protein-nanopores/feed.xml>

`search_queries` unions up to four configured OpenAlex Boolean title/abstract searches
with author/journal results. Each query is limited to 512 bytes and 2,000 candidate works
(or the lower `GSRF_MAX_OPENALEX_WORKS` limit). Date and optional topic filters apply to
each search. Search-only feeds are supported; `topics` alone still does not discover works.
Paging, authentication, version deduplication, and required-refresh failure handling use the
existing OpenAlex pipeline. Exceeding a limit fails publication instead of truncating it.
Searches also have a page-request budget, including a final page to confirm exhaustion.

`relevance_filter = "protein_nanopore"` requires pore, protein/peptide analyte, and
measurement evidence in the title or a single abstract sentence. A protein forming the
pore is not by itself a protein analyte. Clear title evidence suffices without an abstract;
otherwise missing abstracts are explicitly logged as exclusions. DNA/RNA-focused titles
without protein-measurement evidence are excluded, but protein measurements using DNA
adapters are allowed. Inclusions log the provider ID and matching evidence; each refresh
reports candidate/accepted counts. This conservative lexical gate can miss vague titles,
missing metadata, and unusual terminology; it is not a completeness guarantee.

Discovery includes CsgG/CsgF, CytK/cytotoxin K, FraC/fragaceatoxin C, and both
hemolysin spellings, alongside MspA and aerolysin. Protein/proteoform and modification
terms are complemented by an electroosmotic-flow, motor-free, and DNA-peptide transport
search. Identification, fingerprinting, variant discrimination, PTM detection, and
quantification are relevant even when a paper does not claim de novo sequencing.
Generic pore engineering and routine DNA/RNA sequencing remain excluded.

### Nanopore foundations and signal analysis

The separate `nanopore-foundations` feed preserves an all-history curated reading list,
without broad author searches or the protein-analyte gate. It includes the HMM primer,
segmentation, Chiron/CTC, DeepNano/RNNs, reviews, protein-reading landmarks, and recent
examples of each experimental approach. This keeps transferable DNA signal-analysis
methods available without admitting routine DNA sequencing into `protein-nanopores`.

The 19 DOI-backed seed records live in
[`src/curated/nanopore-foundations.json`](./src/curated/nanopore-foundations.json).
Titles, authors, and dates were checked against Europe PMC, Crossref, and bioRxiv metadata;
author IDs in `feeds.toml` were checked against OpenAlex landmark-paper authorships.
The collection is bundled into the binary, uses the existing curated-source enrichment
and deduplication pipeline, and retains bibliographic records if enrichment is unavailable.
Categories describe approaches and demonstrated capabilities, not equivalent claims of
sequencing accuracy. Preprints are labeled explicitly, including the Research Square
CytK paper; PMC availability alone does not imply peer review.

Once a new snapshot is published, the reader and RSS paths are
`/feeds/nanopore-foundations/` and `/feeds/nanopore-foundations/feed.xml`.
Edit the seed JSON to maintain the collection; the all-history cutoff keeps older papers
visible. Wikipedia, blog posts, and software announcements are not represented as papers.
This does not add a general Research Square crawler or a live methods-discovery feed.

### OpenAlex access

Set `GSRF_MAILTO` to a real contact address before production use. OpenAlex uses it for its
polite API pool; it is sent to OpenAlex but is not included in the RSS feed or baked into the
container image.

Set `GSRF_OPENALEX_API_KEY` privately for production feeds and static publication. Get a
verified free key from [OpenAlex Settings](https://openalex.org/settings/api-key); see
[OpenAlex authentication](https://help.openalex.org/api/authentication/). The contact address
does not replace authentication. The key is sent only to the HTTPS OpenAlex API as a sensitive
Authorization header, never as a query parameter or exported metadata. All OpenAlex lookups,
pagination, and curated enrichment use the same helper.

A bounded single-page feed can still run without a key. Larger feeds may fail with HTTP 401
when following the first cursor; the full-config local migration rehearsal encountered
exactly this requirement. Provide the key through a private environment or uncommitted
Compose `.env`, not a PR, image build argument, public repository, or chat message.
OpenAlex work pages use the documented 100-result maximum. Rate/budget errors remain
explicit rather than silently truncating output or retrying indefinitely.

```toml
default_feed = "myfield"           # served at bare "/"

[settings]
from_days = 365                    # default recency window

[feeds.myfield]
title = "Machine Learning & Synthetic Biology"

[[feeds.myfield.people]]
name = "David Baker"
openalex_id = "A5135542215"

[[feeds.myfield.people]]
name = "Jeff Nivala"
openalex_id = "A5005023517"

[feeds.synbio]
title = "Synthetic Biology"
author_ids = ["A5135542215", "A5005023517"]

# Journal-only feeds are also supported.
[feeds.top-journals]
title = "Top Journals"
source_ids = ["S137773608", "S64187185"]
```

Per-feed keys:

| Key | Purpose |
|---|---|
| `title` | Optional RSS channel title |
| `people` | Canonical author records shared by all providers |
| `curated_sources` | Curated paper collections merged into the feed |
| `biorxiv_categories` | Native bioRxiv categories merged into the feed |
| `arxiv_categories` | Native arXiv categories merged into the feed |
| `orcids` | ORCIDs resolved to OpenAlex author IDs |
| `authors` | Author names resolved by top search result; imprecise for common names |
| `source_ids` | OpenAlex journal/source IDs; preferred |
| `issns` | ISSNs resolved to OpenAlex source IDs |
| `journals` | Journal names resolved by top search result |
| `topics` | OpenAlex topic IDs used to narrow results |
| `from_days` | Feed-specific rolling recency window in days |
| `from` | Explicit earliest publication date in `YYYY-MM-DD` format |

Each `people` entry accepts:

| Key | Purpose |
|---|---|
| `name` | Required canonical name and default Google Scholar query |
| `openalex_id` | Optional precise OpenAlex author ID |
| `google_scholar_name` | Optional Scholar query spelling when it differs from `name` |
| `optional` | Defaults to `false`; set to `true` to leave this author unchecked in the reader on initial load, without removing their papers from RSS |

For example, an author can remain available in the feed without being selected initially:

```toml
[[feeds.myfield.people]]
name = "David Baker"
openalex_id = "A5135542215"
optional = true
```

The reader checks non-optional authors by default and hides papers whose tracked authors are
all optional. Papers coauthored by a non-optional tracked author remain visible, as do papers
from journals, curated collections, or preprint categories with no tracked author. Optional
authors remain in the dropdown and can be selected at any time. This applies to both providers
and name-matched curated/preprint records; raw RSS still contains all authors' papers.
Google Scholar retains its query-to-author match, so recognized abbreviated names also
inherit the configured author's optional status after duplicate versions are merged.
Exact author-name matches preserve given-name order while accepting explicit surname-first
notation (`Smith, John A`). Otherwise, compatible name variants (including omitted middle
initials) are associated only when one surviving author qualifies. Conflicting names or
initials are rejected, and ambiguous associations remain unresolved.
OpenAlex ID matches are also retained before versions are merged. Exact IDs take precedence;
if a richer version replaces an ID, its author can inherit the original identity only through
an unambiguous compatible name match, never through an ID belonging to another tracked person.
Optional status comes from the retained identity rather than an initial-insensitive name.
Name-only optional matching is used when identifiers cannot be compared and no resolved
provider association is available.
When several query identities or filter entries coincide, non-optional defaults take
precedence: an author association or shared checkbox is optional only if all its matched
identities or entries are optional. Shared name-based filter IDs still select those authors
together when explicitly chosen.

The legacy `author_ids`, `authors`, and `google_scholar_authors` arrays remain supported for
existing configuration files, but new feeds should use `people`.

A feed requires at least one author or journal. If both are present, results are the **union**
of the authors' publications and recent publications in the journals. Results are
de-duplicated and sorted newest-first. A topic filter applies to both sides of the union.

OpenAlex sometimes stores a preprint and its published paper as separate works, and it
occasionally mints two author entities for the same person. The feed groups records when they
share a DOI, or when their normalized titles match **and** either their complete OpenAlex
author-ID sets or their normalized author-name sets match. Name comparison is order-, case- and
initial-insensitive (`Tang, Sophia` == `S. Tang`). It retains the version with an open PDF, then
an abstract, then published status, then the newest date. Links to the other versions are
included in the retained item's description. Records whose titles or author lists cannot be
compared are kept separate rather than risking a false match.

The config is read on every request. A missing file enables ad-hoc-param mode, while an unreadable
or malformed file returns an HTTP 500 response instead of silently disabling configured feeds.
Identical resolved queries are cached for up to one hour. Restarting clears generated feeds and
in-memory provider data, while the curated source snapshot survives through `GSRF_CACHE_DIR`. In
Docker, changing the repository copy of `feeds.toml` requires publishing and deploying a new image
because the file is baked into that image.

### Switching providers

OpenAlex is the default and recommended provider. To use the archived Google Scholar scraper,
set:

```env
GSRF_PROVIDER=google-scholar
```

Then restart or recreate the application. The public reader and RSS URLs do not change. Set
`GSRF_PROVIDER=openalex` and restart to switch back.

Each named feed intended to support both providers should define `people` records. Scholar uses
each person's canonical `name` unless `google_scholar_name` overrides it, while OpenAlex uses
`openalex_id`. The production `bioml` feed has been migrated to this format. Ad-hoc `?author=`
values also work with the Google Scholar provider. Journal, source, ORCID, and topic filters
are OpenAlex-only.

Google Scholar does not provide a supported public API. This fallback uses the project's
original unofficial HTML scraper and can be blocked or broken by markup changes. Provider
errors return HTTP 502 and are logged rather than silently returning an empty feed. The switch
is intentionally explicit instead of automatic so an OpenAlex outage cannot unexpectedly
trigger many Google Scholar requests. Google Scholar exposes only a publication year in these
results, so the `from` cutoff is truncated to that year when this provider is active.

### Curated protein-design papers

The default `bioml` feed includes recent stable-ID papers from
[Peldom/papers_for_protein_design_using_DL](https://github.com/Peldom/papers_for_protein_design_using_DL):

```text
http://localhost:3005/
http://localhost:3005/?rss
```

The named `protein-design-archive` feed retains the complete accepted history:

```text
http://localhost:3005/?feed=protein-design-archive
http://localhost:3005/?feed=protein-design-archive&rss
```

This source is independent of the selected OpenAlex or Google Scholar provider. The default
feed applies its normal one-year cutoff before merging and deduplicating curated and provider
records. Tracked authors are highlighted on curated-only records using normalized exact-name
matching against canonical names and configured aliases.

Curated ingestion includes entries with an explicit DOI, bioRxiv DOI (`10.1101` or
`10.64898`), or arXiv ID.
Entries without a stable identifier and entries marked unavailable are skipped. Benchmarks and
datasets (section 0), small-molecule models (section 7.3), and unclassified commercial reports
(section 7.5) remain intentionally excluded after Phase 4 review. No-ID entries are not matched
by title because an incorrect scholarly match is worse than an omitted record; selected entries
may be added later only after manually verifying a DOI or arXiv identifier.

The upstream Markdown is fetched at most once every 24 hours with conditional ETag requests,
a 2 MiB response limit, and the shared provider timeouts. Invalid updates do not replace a
successfully parsed snapshot. The archive displays upstream section provenance and links back
to the curated source; the GPL-licensed README itself is not copied into this repository.
Publication dates preserve the precision available in the source list: bioRxiv identifiers
provide an exact day, modern arXiv identifiers provide a year and month, and citations that
only state a year use January 1 as an explicit year-only placeholder.

If an accepted paper has no publication date, the service reads the public GitHub blame data
for that paper's title line and records the associated commit date separately. The reader
labels this fallback **Added to collection** and links to the commit; it is never presented as
the publication date or emitted as the RSS publication date. The collection date can still
qualify and sort an otherwise undated paper within a recent feed. Blame metadata is fetched
only when undated entries exist and is cached with the 24-hour curated-source snapshot.

When OpenAlex is the active provider, curated DOI and arXiv identifiers are resolved in bounded
batches to add canonical dates, abstracts, venues, open-access PDFs, and publication-version
relationships. Matching uses stable identifiers only. If enrichment fails, the direct curated
records remain available and the error is logged.

Set `GSRF_CACHE_DIR` to persist the complete parsed source snapshot, including collection-date
provenance, across restarts. Docker uses `/cache`; both Compose files mount the
`scholar-rss-cache` named volume there. Snapshot replacement is atomic, malformed or undersized
snapshots are rejected, and a network failure uses the last successfully parsed snapshot.
Persistent discovery snapshots use a versioned envelope that records their source, schema,
refresh time, coverage date, works, and source-specific cursor metadata. Native repository
integrations can therefore refresh overlapping date windows, merge duplicates, and enforce
retention without rebuilding their full history.

Curated coverage can be evaluated independently of generated feed output:

```sh
cargo run -- --compare-curated bioml peldom-protein-design --config feeds.toml
```

The command fetches the configured provider records and compares them with recent
curated papers using the feed's normal cutoff. Its Markdown report includes DOI and
title/author overlap, unique curated discoveries, version-merged volume, metadata gaps,
provider enrichment opportunities, section distribution, and all-time parser exclusions. It
does not modify configuration or feed output. Known false positives found during manual review
are excluded by stable identifier and reported separately from structural parser exclusions.
Set `GSRF_PROVIDER` to compare against the explicit Google Scholar fallback instead of OpenAlex.

### Finding reliable OpenAlex IDs

Prefer IDs over names because author profiles can be conflated or fragmented:

```text
https://api.openalex.org/authors?search=Jeff%20Nivala
https://api.openalex.org/sources?search=Nature%20Communications
https://api.openalex.org/topics?search=synthetic%20biology
```

OpenAlex URLs and bare IDs are both accepted and normalized, for example
`https://openalex.org/A5005023517` and `A5005023517`.

## Feed URLs

After starting the service:

- Default configured reader: `http://localhost:3005/`
- Default configured raw RSS: `http://localhost:3005/?rss`
- Named feed reader: `http://localhost:3005/?feed=myfield`
- Named feed raw RSS: `http://localhost:3005/?feed=myfield&rss`
- Protein-design archive: `http://localhost:3005/?feed=protein-design-archive`
- Multiple ad-hoc authors:
  `http://localhost:3005/?author_id=A5135542215&author_id=A5005023517`
- Default reader with optional native preprints:
  `http://localhost:3005/?biorxiv_category=synthetic_biology&arxiv_category=q-bio.BM`
- The same optional sources as raw RSS:
  `http://localhost:3005/?biorxiv_category=synthetic_biology&arxiv_category=q-bio.BM&rss`
- Preprints only:
  `http://localhost:3005/?paper_sources=custom&paper_source=biorxiv:synthetic_biology&paper_source=arxiv:q-bio.BM`

All identifier parameters are repeatable and are merged with a selected named feed:

| Parameter | Description |
|---|---|
| `feed` | Name under `[feeds.<name>]` in `feeds.toml` |
| `author_id` | OpenAlex author ID |
| `orcid` | ORCID resolved to an OpenAlex author |
| `author` | Author name resolved using the top search result |
| `source_id` | OpenAlex journal/source ID |
| `issn` | ISSN resolved to an OpenAlex source |
| `journal` | Journal name resolved using the top search result |
| `topic` | OpenAlex topic ID used to constrain results |
| `concept` | Alias for `topic` |
| `from` | Earliest publication date, `YYYY-MM-DD` |
| `biorxiv_category` | Repeatable optional bioRxiv category; none are selected by default |
| `arxiv_category` | Repeatable optional arXiv category; none are selected by default |
| `native_days` | Native-source window from 1–30 days; defaults to 7 for query-selected categories |
| `paper_sources` | Set to `custom` when explicitly choosing paper sources |
| `paper_source` | Repeatable source selection generated by the reader: `core`, `curated:<key>`, `biorxiv:<category>`, or `arxiv:<category>` |
| `view_period` | Browser-only date filter: `30d`, `90d`, or `1y` |
| `view_author` | Repeatable browser-only tracked-author filter generated by the reader controls |
| `view_authors` | Browser-only author selection mode: `default`, `custom` (including an empty selection), or `all` |
| `view_author_selection` | Internal fixed-length fingerprint of the form's original author selection; distinguishes author edits from source changes, including without JavaScript |
| `view_author_options` | Internal fixed-length fingerprint of the form's original available authors; recognizes select-all edits without JavaScript independently of source changes |
| `view_source` | Legacy browser-only curated-collection result filter |
| `rss` | Return raw RSS XML instead of the browser reader |

The browser reader is server-rendered HTML with no third-party assets. A small first-party script
applies single-select filters when they change and multiselect filters when they close; the same
controls retain an Apply button when JavaScript is unavailable. Date and author controls narrow
already-loaded publications without another provider request. The **Paper sources** control
chooses which discovery pipelines contribute records and therefore refreshes the feed. Its core
provider and configured curated collection are selected by default; native categories are not.
Unchecking both core sources and choosing native categories produces a preprint-only reader.
Multiple selected authors and paper sources use OR semantics within their respective groups.
The author checkboxes start with all non-optional authors selected. Selecting **All tracked
authors** includes optional authors and papers with no tracked author; clearing every checkbox
in a custom selection shows no papers. Other custom author selections show only matching papers.
Date/source changes preserve the initial author defaults until the author selection changes.
An explicit **All tracked authors** selection also remains unrestricted when new paper sources
introduce additional authors.
An unchanged custom selection does not become unrestricted when a source has fewer authors.
Without JavaScript, checking every author in the original form also selects **All tracked authors**.
**Clear filters** restores the configured author defaults. Author selections are encoded in the
URL, not browser storage, and are removed from raw RSS links.
Reader selections are preserved when opening an article and returning to the reader.
Paper-source selections also carry over to raw RSS. At least one paper source remains selected;
an empty no-JavaScript submission restores the configured defaults. Parameter-free reader and RSS URLs retain the
focused default feed.

Available bioRxiv values are `synthetic_biology`, `bioinformatics`, `bioengineering`,
`systems_biology`, `genomics`, `biochemistry`, and `molecular_biology`. Available arXiv values
are `q-bio.BM`, `q-bio.QM`, `q-bio.GN`, `q-bio.MN`, `q-bio.PE`, and `physics.bio-ph`.
Unfiltered `cs.LG` is intentionally unavailable because its volume and subject breadth would
overwhelm the focused feed.
Selecting a publication opens an internal preview page
on a unique `/article/<id>` path with its abstract and links to the original article and any
available open-access PDF. Unique paths prevent browser reading modes from reusing a different
page's extracted content. Tracked authors are highlighted in both views using provider IDs when
available and canonical names or configured aliases for curated records; if several tracked
authors contributed, each is highlighted.
Raw RSS has CORS enabled. The generated channel has a 60-minute TTL, and the in-memory cache is
cleared hourly.

## Running locally

### Native Rust

Install Rust with `rustup`, then run:

```sh
cargo run
# http://127.0.0.1:3005/?feed=myfield
```

The optional positional argument changes the bind address, and `--config` changes the config
path:

```sh
cargo run -- "0.0.0.0:3005" --config feeds.toml
GSRF_CONFIG=feeds.toml GSRF_MAILTO=you@example.com GSRF_PROVIDER=openalex cargo run
```

Useful development checks:

```sh
cargo test
cargo clippy --all-targets --all-features
node --test tests/reader.test.cjs # Reader controls; uses Node's built-in test runner.
```

The test suite uses local fixtures and constructed provider records; it does not contact
OpenAlex or Google Scholar. It covers provider selection, Google Scholar result conversion,
feed configuration, curated-source parsing and evaluation, version deduplication,
provider-neutral author highlighting, RSS conversion, HTML escaping, and article previews.

### Local Docker Compose

[`local/docker-compose.yml`](./local/docker-compose.yml) publishes port `3005` and can either
pull the GHCR image or build the current checkout.

Pull and run the published image:

```sh
docker compose -f local/docker-compose.yml pull
docker compose -f local/docker-compose.yml up -d
```

Build and run the current source:

```sh
docker compose -f local/docker-compose.yml up -d --build
```

Inspect or stop it:

```sh
docker compose -f local/docker-compose.yml logs -f
docker compose -f local/docker-compose.yml down
```

The local Compose file contains a commented bind mount for `feeds.toml`. Enable it when config
changes should be visible without rebuilding the image; restart the container to clear any
cached feed immediately. Set `GSRF_MAILTO` and, optionally, `GSRF_PROVIDER` in your shell or in
a repository-root `.env` file before starting Compose.

## GitHub Actions and GHCR

The [`Publish Docker image`](./.github/workflows/docker-publish.yml) workflow runs when:

- A commit is pushed to `main`
- A tag beginning with `v` is pushed
- It is manually started from **GitHub → Actions → Publish Docker image → Run workflow**

It uses the repository's built-in `GITHUB_TOKEN`; no registry secret is required. Published
tags include `latest` for `main`, branch/semantic-version tags where applicable, and an
immutable `sha-...` tag for each build. The image targets `linux/amd64`, matching the Synology
DS423+.

After pushing a change, verify the workflow succeeded in the repository's **Actions** tab
before expecting the NAS to find a new image.

### Package visibility

For anonymous NAS pulls, set the package to public once:

**GitHub profile → Packages → scholarly-rss-feed → Package settings → Change visibility
→ Public**

To keep it private instead, authenticate the NAS using a GitHub personal access token with
`read:packages`:

```sh
echo "YOUR_PAT" | /usr/local/bin/docker login ghcr.io \
  --username Michaelmvh --password-stdin
```

## Synology NAS and Cloudflare Tunnel

The production file is [`NAS/docker-compose.yml`](./NAS/docker-compose.yml). It runs:

- `scholar-rss`: the prebuilt GHCR application image, available only to the Compose network.
- `scholar-rss-tunnel`: Cloudflare's connector, which provides public HTTPS access.

No router port forwarding is needed, and port `3005` is not published on the NAS.

### Prerequisites

- Synology Container Manager installed on the DS423+.
- The GHCR package is public, or the NAS is authenticated to GHCR.
- `michaelmvh.com` is active in Cloudflare DNS.
- A Cloudflare Zero Trust tunnel and token.

### GitHub Pages DNS safety

The existing GitHub Pages site can coexist with the tunnel:

- Keep the four GitHub Pages `A` records and the `www` CNAME set to **DNS only** (grey cloud).
- Keep mail records such as MX, SPF, DKIM, and domain-verification records DNS only.
- Only the tunnel hostname, `reading.michaelmvh.com`, should be proxied by Cloudflare.
- The tunnel hostname is independent of the apex and `www` records used by GitHub Pages.

### First-time deployment

1. In **Cloudflare Zero Trust → Networks → Tunnels**, create or open the tunnel.
2. Add a published application/public hostname:
   - Hostname: `reading.michaelmvh.com`
   - Service type: `HTTP`
   - Service URL: `scholar-rss:3005`
3. Copy only the tunnel token from Cloudflare's installation command. Do not run that command;
   the Compose file already runs `cloudflared`.
4. In Synology File Station, create:

   ```text
   /volume1/docker/scholar-rss/
   ```

5. Upload [`NAS/docker-compose.yml`](./NAS/docker-compose.yml) directly into that directory.
   No rename is required.
6. Create a plain-text file named `.env` beside it:

   ```env
   TUNNEL_TOKEN=eyJ...
   GSRF_PROVIDER=openalex
   ```

   The resulting directory must contain:

   ```text
   /volume1/docker/scholar-rss/
   ├── docker-compose.yml
   └── .env
   ```

   `.env` may be hidden in File Station. Enable **Settings → General → Show hidden files** if
   necessary.
7. Open **Container Manager → Project → Create**:
   - Project name: `scholar-rss`
   - Path: `/volume1/docker/scholar-rss`
   - Source: existing `docker-compose.yml`
8. Build/start the project and confirm both `scholar-rss` and `scholar-rss-tunnel` are running.
9. Open the browser reader:

   ```text
   https://reading.michaelmvh.com/
   ```

10. Configure the TRMNL RSS plugin with the raw feed URL:

    ```text
    https://reading.michaelmvh.com/?rss
    ```

    The URL remains stable when the default named feed's contents change.

### Automatic image updates

Container Manager does not automatically pull or redeploy a changed `latest` image when a
container is stopped and started. The NAS uses a scheduled user-defined script named
**Update Scholar RSS**:

- Run as: `root`
- Schedule: weekly
- Script:

```sh
cd /volume1/docker/scholar-rss &&
/usr/local/bin/docker compose pull &&
/usr/local/bin/docker compose up -d --remove-orphans
```

This updates both the application and `cloudflared`. It leaves the existing containers running
if pulling an image fails because the commands are chained with `&&`.

For an immediate application update, explicitly pull the image and force recreation; merely
uploading Compose or restarting the existing container can reuse the old local image:

```sh
cd /volume1/docker/scholar-rss &&
/usr/local/bin/docker compose pull scholar-rss &&
/usr/local/bin/docker compose up -d --force-recreate scholar-rss
```

Alternatively, use **Pull** and rebuild the project in Container Manager.

### Updating the Compose configuration

The scheduled task updates images, not `docker-compose.yml`. If
[`NAS/docker-compose.yml`](./NAS/docker-compose.yml) changes:

1. Download or upload the new file from GitHub over the existing NAS copy.
2. Keep the existing `.env`; it contains the tunnel secret.
3. Pull and recreate the project so both the new Compose settings and latest images take
   effect:

   ```sh
   cd /volume1/docker/scholar-rss &&
   /usr/local/bin/docker compose pull &&
   /usr/local/bin/docker compose up -d --force-recreate --remove-orphans
   ```

### Rollback

Every successful workflow build publishes an immutable `sha-...` image tag. To roll back:

1. Find the desired tag on the GitHub package page.
2. Change the NAS Compose image temporarily:

   ```yaml
   image: ghcr.io/michaelmvh/scholarly-rss-feed:sha-abcdef0
   ```

3. Pull and recreate the project.
4. Change it back to `:latest` after the problem is fixed.

## Caching and request load

Normal TRMNL polling will not overwhelm the NAS. Up to 128 generated feeds are cached in memory
for one hour with least-recently-used eviction. A cache hit only clones and serializes the
already-built channel; it does not call a provider again. A cold OpenAlex request makes one
cursor-paginated author query and one cursor-paginated journal query, concurrently. Each query
fails explicitly rather than returning a partial feed if it exceeds 20,000 works; set
`GSRF_MAX_OPENALEX_WORKS` to a different positive ceiling. A cold Google Scholar request makes one
query per configured person, sequentially. Concurrent requests for the same uncached feed share
one in-flight build instead of duplicating provider traffic, and provider requests use finite
connection and total timeouts. The server accepts up to 64 concurrent connections and runs up to
eight cold feed builds at once.

Use stable OpenAlex IDs in `feeds.toml` and give TRMNL a named feed URL such as
`https://reading.michaelmvh.com/?feed=myfield&rss`. Name, ORCID, ISSN, and journal-name parameters
must be resolved before the channel-cache lookup and can therefore cause extra OpenAlex calls.

Dynamic requests are limited to an 8 KiB query string, 100 parameters other than `view_author`,
256 repeated `view_author` parameters, 25 provider/source filter values, and 256 bytes per value.
The total query-size limit still applies to large author selections.
Dynamic feed and reader routes also allow 60 requests per client
IP per minute. `GSRF_TRUST_PROXY_CLIENT_IP=true` enables the Cloudflare connecting-IP header for
this limit and is set only in the tunnel deployment; do not enable it when clients can connect
directly. Inbound headers have a 10-second deadline, HTTP keep-alive is disabled, and excess cold
feed builds receive `503 Service Unavailable` instead of queueing. Each connection has a
five-minute overall deadline so stalled response readers cannot hold capacity indefinitely.
Cloudflare does not automatically cache the dynamic RSS URL; retain edge rate limiting as an
additional layer for the public endpoint.

These limitations are not significant for ordinary TRMNL use, but Cloudflare can cheaply
protect the public endpoint:

1. In Cloudflare, open **Caching → Cache Rules → Create rule**.
2. Name the rule `Cache reading RSS feed`.
3. Use the expression:

   ```text
   (http.host eq "reading.michaelmvh.com")
   ```

4. Set **Cache eligibility** to **Eligible for cache**.
5. Set an **Edge TTL** of two hours. Dynamic responses tell browsers to revalidate immediately,
   while `Cloudflare-CDN-Cache-Control` allows Cloudflare to retain a response for two hours and
   serve stale content during background revalidation.
6. Keep query strings in the cache key so different named feeds remain separate.

Cloudflare's first request for a URL is a cache miss and reaches the NAS; subsequent requests
for that exact URL are served at the edge until expiration. Verify the rule by requesting the
same URL twice and inspecting `CF-Cache-Status`:

```sh
curl -sI "https://reading.michaelmvh.com/?feed=myfield" | grep -i cf-cache-status
curl -sI "https://reading.michaelmvh.com/?feed=myfield" | grep -i cf-cache-status
```

The first response should normally be `MISS` and a later response `HIT`. `DYNAMIC` or `BYPASS`
means the Cache Rule is not applying.

After deploying a feed change that must appear immediately, use **Cloudflare → Caching →
Configuration → Purge Cache → Custom Purge** for the feed URL. Otherwise, allow up to the edge
TTL for the cached response to expire.

Edge caching protects repeated URLs, but unique query strings can still force cache misses.
As an additional abuse safeguard, create a Cloudflare rate-limiting rule:

1. Open **Security → WAF → Rate limiting rules**.
2. Match hostname `reading.michaelmvh.com`.
3. Set a limit such as 60 requests per minute per client IP.
4. Block or challenge the client briefly after exceeding the limit.

This threshold is far above normal RSS polling while preventing one client from continuously
generating unique ad-hoc queries. Availability and exact limits depend on the Cloudflare plan.
Do not place Cloudflare Access login in front of the feed unless the TRMNL client is configured
to supply the required credentials.

## Security and recovery

- Never commit `.env` or the Cloudflare tunnel token. `.env` is ignored by Git.
- If the token is exposed, rotate it in Cloudflare and replace the NAS `.env` value.
- The public RSS endpoint has no application authentication. Do not put private information in
  feed titles or configuration.
- Do not open or forward port `3005` on the router; the tunnel is the public entry point.
- Repository code, feed definitions, Compose configuration, and image history are stored on
  GitHub/GHCR. The only NAS-specific secret to preserve is `.env`.
- To recover on a replacement NAS, install Container Manager, upload
  `NAS/docker-compose.yml`, create `.env` with a valid tunnel token, and create the project.

## Troubleshooting

| Symptom | Resolution |
|---|---|
| GHCR pull returns `unauthorized` or `denied` | Make the package public or run `docker login ghcr.io` with a token containing `read:packages`. |
| Compose reports `TUNNEL_TOKEN` is unset | Confirm `.env` is beside `docker-compose.yml`, is named exactly `.env`, and contains `TUNNEL_TOKEN=value` without spaces around `=`. |
| Tunnel shows disconnected | Check the `scholar-rss-tunnel` logs and confirm its token is current. |
| Cloudflare returns HTTP 502 | Confirm `scholar-rss` is running and the Cloudflare service URL is exactly `http://scholar-rss:3005`, not `localhost`. |
| Public hostname has a certificate warning during DNS setup | Wait until the domain is active. GitHub Pages records should remain DNS only; the tunnel hostname should be proxied. |
| Feed content appears stale | Feeds are cached for up to one hour. Restart `scholar-rss` to clear the cache immediately. |
| A pushed feed change does not appear | Confirm GitHub Actions succeeded, then confirm the NAS pulled and recreated the new `latest` image. |
| A local `feeds.toml` change does not appear in Docker | Rebuild the image, or enable the bind mount in `local/docker-compose.yml` and restart the container. |
| An author returns unrelated papers | Replace name lookup with a precise OpenAlex author ID or ORCID, and optionally add topic IDs. |
