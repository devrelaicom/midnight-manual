# Midnight Manual — landing site · design

- **Date:** 2026-06-03
- **Status:** Approved (brainstorm complete; ready for implementation planning)
- **Topic:** Single-page product/marketing/docs landing site for `midnight-manual`, hosted on GitHub Pages.

---

## 1. Goal

Build one long, beautiful, single static page that serves as the primary landing page for `midnight-manual` — a combined product / promo / marketing / docs page. It tells a leaner, less-dense version of the [README](../../../README.md) story and drives one primary action: **add the MCP server / install the CLI**.

### Non-goals

- Not a documentation site or multi-page site. One page, deep-linkable sections.
- Not a replacement for the README, the spec, or the GitHub repo. It *links* to them for depth.
- No backend, no framework, no build step (see §6).

### Source of truth

Every factual claim about the product (tool counts, model names, commands, behaviours) comes from the **README** and the repo, never from model memory. The README is canonical. Example query strings shown on the page (e.g. `"sealed ledger"`, `"nullifier double-spend prevention"`) are reused verbatim from the README — the page does not author new Compact/SDK code or technical Midnight claims.

---

## 2. Visual system — "Blueprint Editorial"

A synthesis of the three references: the loud editorial scale of *The Summer Drive*, the thin technical line-work of an *exploded blueprint diagram*, and the monospace-label / open-source feel of the *Hermes Agent* site. Deliberately **light/warm**, not a dark "crypto" theme — matching the references the design is built from.

### Color tokens

| Token | Value | Use |
| --- | --- | --- |
| `--paper` | `#F4EFE3` | Page ground (warm cream) |
| `--ink` | `#1F3DF5` | Primary — electric blueprint blue (headlines, lines, links, callouts) |
| `--graphite` | `#2A2A28` | Body text |
| `--accent` | `#E5392F` | Registration red — used **sparingly** (ticks, the one "live" marker) |
| `--paper-2` | tint of `--paper` (e.g. `#EDE7D9`) | Subtle section banding |
| `--ink-soft` | `--ink` @ ~50–70% | Hairlines, leader lines, secondary mono |

> At build time, confirm whether Midnight's official brand blue differs from `#1F3DF5`; if so, adopt the brand value as `--ink`.

### Typography (self-hosted `.woff2`, no runtime third-party request)

| Role | Face | Notes |
| --- | --- | --- |
| Display headlines | **Anton** | Giant, uppercase, tight line-height. Hero + section headers. Display-only (single weight). |
| Subheads / body / UI | **Space Grotesk** | The workhorse family (weights 400/500/700). |
| Labels / code / callouts | **JetBrains Mono** | Eyebrows, blueprint callouts, command strips, terminal demo. |

### Motif

Thin blueprint-blue line-art with monospace callouts on right-angle leader lines. Used **once at hero scale** (the exploded Retrieval Engine) and as small, repeating SVG section markers elsewhere. **Do not reuse** the raster banner PNGs in `docs/assets/readme/` — they carry the README's aesthetic and would clash; the site uses its own inline SVG line-art.

**Vector vs. raster split:** functional line-art (the engine diagram, section markers, icons) is **hand-authored SVG** — crisp, recolorable via tokens, and animatable. Decorative **raster** assets (the OG/share image, any imagery) are generated with the `image-gen:nanobanana` skill in the Blueprint Editorial palette.

### Motion (restrained & elegant)

- Exploded engine performs **one** gentle assemble-on-load (parts settle into place, leader lines draw in).
- Sections fade / slide in on scroll via `IntersectionObserver`.
- Smooth hover transitions; copy-to-clipboard buttons on command strips.
- **`prefers-reduced-motion: reduce`** → all of the above collapse to static final states. No motion is load-bearing for comprehension.

---

## 3. Page structure (top → bottom)

The hero uses the **split** layout (headline + install left, engine right). Quick start is deliberately high on the page.

1. **Hero** — Split.
   - *Left:* eyebrow (mono: `PRE-PRODUCTION · RUST 1.91 · LOCAL MODELS`), Anton headline, one-line subhead, primary install strip with copy button (`claude mcp add midnight-manual -- mnm mcp serve`), two pill CTAs: **Install** (anchors to Quick start) and **View on GitHub** (↗).
   - *Right:* exploded **Retrieval Engine** diagram — blueprint discs along a diagonal axis with mono callouts (`embed`, `hybrid · lexical+vector`, `RRF · k=60`, `rerank → confidence`).
2. **Quick start** *(moved high, per decision)* — three numbered steps:
   1. Build from source (`git clone` → `cargo build --release -p mn-cli` → put `mnm` on `PATH` → `mnm doctor`).
   2. Add to your client — **tabs: Claude Code / Codex / Cursor**, each showing the right snippet.
   3. First search — a **live-styled terminal** mock showing `mnm search "…"` returning a ranked, source-attributed result with a confidence score.
3. **The pitch** — one tight band explaining *why*: stale training data vs. grounded, cited, private retrieval.
4. **The Retrieval Engine** — the centerpiece explained in prose + the diagram's callouts: hybrid (PostgreSQL full-text + pgvector) → **RRF (k=60)** → optional `bge-reranker-base` cross-encoder → **confidence = trust × relevance**, with the trust factors (attribution, verification, freshness, deprecation, version-match).
5. **Three tools, one binary** — three blueprint cards: **MCP server**, **CLI (`mnm`)**, **cloud corpus** (the hosted instance is the compiled-in default; most users never run a server).
6. **In your AI client** — the MCP server: **11 tools** grouped as *Search* · *Read-in-context* · *Corpus & models*; lazy model loading (sub-500ms cold start); structured self-correcting errors (`embedding_model_mismatch` → `pull_models`).
7. **It teaches the technique** — the **Advanced Search Skill** as a compact grid (HyDE, multi-query, step-back, lexical anchoring, symbol-aware code search, retrieve-read-retrieve, trust-weighted selection, cross-source comparison), plus the one-line install (`/mnm:add-advanced-search-skill` or `mnm skills add`).
8. **The smart chunker** — heading-aware (Markdown) + symbol-aware (tree-sitter) chunking; **Compact is a first-class citizen**; the supported-language grid; graceful fallback for unknown grammars.
9. **Local & private** — embeddings/reranking run locally (no API key, no account); opt-out telemetry (env / config / runtime) carrying no content; the CI privacy canary that fails any build leaking a forbidden string.
10. **Footer** — honest pre-production note + links: **GitHub** (`https://github.com/devrelaicom/midnight-manual`) · spec (`specs/001-rag-platform/spec.md`) · constitution (`CONSTITUTION.md`) · cookbook (`docs/cookbook/query-enhancement.md`). The "View on GitHub" hero CTA points at the same repo URL.

### Explicitly out of scope (cut from the README for this page)

Admin & operations, the ingestion pipeline, rate-limits & uplift, self-hosting/server setup, and the full configuration reference. Readers who want these follow the GitHub link. No one-line callouts for them either.

---

## 4. Copy

Drafted in the README's voice — confident, technical, a little playful (the README signs off "Build something anyway"). All copy below is **proposed and editable** at the review gate.

- **Eyebrow:** `PRE-PRODUCTION · RUST 1.91 · LOCAL MODELS`
- **Hero headline:** **Ask your docs, not your model.**
- **Hero subhead:** *A privacy-respecting retrieval engine for the Midnight Network — so your AI assistant answers from the real docs and source, not a stale training set.*
- **CTAs:** `Install` · `View on GitHub`
- **Pitch band:** *Your assistant's training data went stale the day it shipped. midnight-manual gives it hybrid search over the live Midnight corpus — ranked, reranked, and cited — with the embeddings running on your machine, so your queries stay yours.*
- **Section headers (Anton, uppercase):** `QUICK START` · `THE RETRIEVAL ENGINE` · `THREE TOOLS, ONE BINARY` · `IN YOUR AI CLIENT` · `IT TEACHES THE TECHNIQUE` · `THE SMART CHUNKER` · `LOCAL & PRIVATE`
- **Footer note:** *Pre-production software for the Midnight Network. The hosted corpus resets without notice and interfaces move. Build something anyway.*

> Optional: run final copy through a voice skill (`midnight-docs-writer`) if a house voice is preferred over the README voice.

---

## 5. Information architecture details

- **Sticky mini-nav (optional, light):** a thin top bar that appears after the hero scrolls away, with jump links to the major sections and a persistent **Install** button. Keep it minimal and on-brand (mono labels). Mark as a nice-to-have, not required for v1.
- **Anchors:** every section has a stable `id` for deep-linking and for the hero/nav CTAs.
- **Code/command blocks:** monospace, blueprint-blue on paper, with copy buttons. The "terminal demo" is styled HTML (the only place near-black is allowed — a small dark terminal panel — as a deliberate contrast beat).

---

## 6. Tech architecture & deploy

**Plain static, no build step.** Lives in a dedicated `site/` directory so it never collides with the existing `docs/` tree or the Rust workspace.

```text
site/
├── index.html            # the whole page
├── css/
│   └── styles.css         # design tokens + layout + components
├── js/
│   └── main.js            # IntersectionObserver reveals, tabs, copy buttons, engine load anim
├── fonts/                 # self-hosted Anton, Space Grotesk, JetBrains Mono (.woff2)
├── img/                   # favicon, OG image, any raster assets
└── CNAME                  # manual.midnightntwrk.expert
```

- **Asset paths are relative**, so the page works at any base path (custom domain or `*.github.io/...`).
- **Custom domain:** `CNAME` contains `manual.midnightntwrk.expert`. DNS is configured by the maintainer (out of scope for this repo). Enforce HTTPS in Pages settings.
- **Deploy workflow:** `.github/workflows/pages.yml`
  - Trigger: `push` to `main` filtered to `site/**` (plus `workflow_dispatch`).
  - Permissions: `pages: write`, `id-token: write`.
  - Steps: `actions/checkout` → `actions/configure-pages` → `actions/upload-pages-artifact` (path: `site/`) → `actions/deploy-pages`.
  - Uses the modern artifact-based Pages deploy — **no `gh-pages` branch**, does not touch `docs/` or interfere with existing CI workflows.
- **Repo Pages settings:** Source = "GitHub Actions".

### Quality bar

- **Responsive:** mobile → wide. The split hero stacks on narrow screens; the engine diagram scales/simplifies.
- **Accessibility:** semantic landmarks, sufficient contrast (blue-on-cream verified for body sizes), visible focus states, `prefers-reduced-motion` support, alt text / `aria` on the SVG diagram.
- **Performance:** no framework, no runtime third-party requests; fonts self-hosted and `preload`ed; SVG (not raster) for line-art; lazy-load any heavy raster; target a fast Lighthouse score.
- **SEO / sharing:** `<title>`, meta description, Open Graph + Twitter card meta, an OG image, favicon, `lang`, canonical URL.

---

## 7. Acceptance criteria

1. `site/index.html` renders the full 10-section page in the Blueprint Editorial system (correct tokens + the three typefaces, self-hosted).
2. The hero is the split layout with a working copy button and a rendered exploded Retrieval Engine SVG with callouts.
3. Quick start sits near the top with working client tabs (Claude Code / Codex / Cursor) and a styled terminal demo.
4. All product claims trace to the README; out-of-scope sections are absent.
5. Restrained motion works and fully degrades under `prefers-reduced-motion`.
6. The page is responsive and passes a basic a11y/contrast pass.
7. `site/CNAME` contains `manual.midnightntwrk.expert`.
8. `.github/workflows/pages.yml` deploys `site/` via the official Pages actions on push to `main`, without disturbing existing workflows or `docs/`.

---

## 8. Decisions log (from brainstorm)

| Decision | Choice | Rejected alternatives |
| --- | --- | --- |
| Visual direction | **A · Blueprint Editorial** (light/warm) | B · Midnight Duotone (dark/brand-literal); C · Paper Schematic (restrained) |
| Signature visual | **The Retrieval Engine** (exploded query path) | Three-in-one modules; the smart chunker |
| Hero layout | **Split** (headline+install left, engine right) | Editorial masthead; terminal-first |
| Motion level | **Restrained & elegant** | Signature scroll motion; static & fast |
| Build approach | **Plain static, no build** | Vite build; static site generator |
| Hosting | **Custom domain** `manual.midnightntwrk.expert` | Pages default URL |
| Type system | **Anton + Space Grotesk + JetBrains Mono** | Space Grotesk+IBM Plex Mono; Archivo Black+IBM Plex Mono |
| Section spine | Quick start moved high; admin/ingestion/rate-limits/self-host/config **cut** | Full README parity |

---

## 9. Open items for implementation

- Confirm Midnight's official brand blue vs. `#1F3DF5`.
- Generate the OG share image (and any decorative raster) via the `image-gen:nanobanana` skill, in the Blueprint Editorial palette. (Functional line-art stays hand-authored SVG.)
- Decide whether the optional sticky mini-nav ships in v1.
- Finalize headline/section copy at the review gate (optionally via `midnight-docs-writer`).
