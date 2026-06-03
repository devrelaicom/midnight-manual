# Midnight Manual Landing Site — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the single-page `midnight-manual` landing site ("Blueprint Editorial" design) as a no-build static site in `site/`, deployed to GitHub Pages via an Action at `manual.midnightntwrk.expert`.

**Architecture:** One `index.html` composed of 10 sections, styled by hand-written CSS (design tokens in `:root`), with one small vanilla-JS file for progressive enhancement (scroll reveals, client tabs, copy buttons, an engine assemble-on-load). Functional line-art (the exploded "Retrieval Engine", section markers, favicon) is hand-authored SVG; the OG share image is generated with the `image-gen:nanobanana` skill. Deploy is the modern artifact-based GitHub Pages flow — no `gh-pages` branch, does not touch the existing `docs/` tree.

**Tech Stack:** Static HTML5 + CSS (custom properties, fl/grid) + vanilla ES module JS. Self-hosted Google Fonts (Anton, Space Grotesk, JetBrains Mono) as `.woff2`. GitHub Actions (`configure-pages` → `upload-pages-artifact` → `deploy-pages`). Verification via a local static server (`python3 -m http.server`) and the `playwright-skill` for screenshots/behaviour checks.

**Source of truth:** Design = `docs/superpowers/specs/2026-06-03-landing-site-design.md`. All product facts = repo `README.md`. Do not invent product claims.

**Working branch:** `worktree-mnm-website` (already checked out, even with `main`). The deploy workflow triggers on push to `main`, so nothing publishes until this branch is merged.

---

## File Structure

```text
site/
├── index.html              # the whole page (10 sections)
├── css/
│   ├── fonts.css           # @font-face for the 3 self-hosted families
│   └── styles.css          # :root tokens + base + components + sections + responsive + motion
├── js/
│   └── main.js             # ES module: reveals, tabs, copy buttons, engine animation, sticky CTA
├── fonts/                  # anton-400 / space-grotesk-{400,500,700} / jetbrains-mono-{400,500} .woff2
├── img/
│   ├── favicon.svg         # hand-authored blueprint mark
│   └── og.png              # generated via nanobanana (1200×630)
└── CNAME                   # manual.midnightntwrk.expert
.github/workflows/pages.yml # GitHub Pages deploy
```

**Class vocabulary** (defined once in Task 3, reused everywhere — do not redefine per section):
`.container .section .section--band .eyebrow .display .h-section .subhead .lead .mono .callout .btn-pill .btn-pill--solid .cmd .cmd__copy .cards .card .card__title .card__meta .grid-2 .grid-3 .grid-4 .terminal .tabs .tab .tabpanel .marker .footer [data-reveal].is-visible .sticky-cta .engine`

---

## Task 1: Scaffold — structure, fonts, tokens, base CSS, page shell

**Files:**
- Create: `site/index.html`, `site/css/fonts.css`, `site/css/styles.css`, `site/fonts/` (downloaded `.woff2`)

- [ ] **Step 1: Create the directory tree**

```bash
mkdir -p site/css site/js site/fonts site/img
```

- [ ] **Step 2: Download and normalise the three font families as woff2**

Run from the repo root:

```bash
cd site/fonts
curl -sL "https://gwfh.mranftl.com/api/fonts/anton?download=zip&subsets=latin&variants=regular&formats=woff2" -o a.zip && unzip -oq a.zip && rm a.zip
curl -sL "https://gwfh.mranftl.com/api/fonts/space-grotesk?download=zip&subsets=latin&variants=regular,500,700&formats=woff2" -o s.zip && unzip -oq s.zip && rm s.zip
curl -sL "https://gwfh.mranftl.com/api/fonts/jetbrains-mono?download=zip&subsets=latin&variants=regular,500&formats=woff2" -o j.zip && unzip -oq j.zip && rm j.zip
# normalise to deterministic names
mv anton-*-latin-regular.woff2 anton-400.woff2
mv space-grotesk-*-latin-regular.woff2 space-grotesk-400.woff2
mv space-grotesk-*-latin-500.woff2 space-grotesk-500.woff2
mv space-grotesk-*-latin-700.woff2 space-grotesk-700.woff2
mv jetbrains-mono-*-latin-regular.woff2 jetbrains-mono-400.woff2
mv jetbrains-mono-*-latin-500.woff2 jetbrains-mono-500.woff2
cd ../..
ls site/fonts
```

Expected: exactly these files —
`anton-400.woff2  jetbrains-mono-400.woff2  jetbrains-mono-500.woff2  space-grotesk-400.woff2  space-grotesk-500.woff2  space-grotesk-700.woff2`

> Fallback if `gwfh.mranftl.com` is unavailable: fetch the same families from `https://fonts.googleapis.com/css2?...` using a browser `User-Agent`, then download the `.woff2` URLs it lists and rename to the names above.

- [ ] **Step 3: Write `site/css/fonts.css`**

```css
/* Self-hosted display + body + mono. font-display: swap to avoid invisible text. */
@font-face { font-family:"Anton"; font-style:normal; font-weight:400; font-display:swap;
  src:url("../fonts/anton-400.woff2") format("woff2"); }

@font-face { font-family:"Space Grotesk"; font-style:normal; font-weight:400; font-display:swap;
  src:url("../fonts/space-grotesk-400.woff2") format("woff2"); }
@font-face { font-family:"Space Grotesk"; font-style:normal; font-weight:500; font-display:swap;
  src:url("../fonts/space-grotesk-500.woff2") format("woff2"); }
@font-face { font-family:"Space Grotesk"; font-style:normal; font-weight:700; font-display:swap;
  src:url("../fonts/space-grotesk-700.woff2") format("woff2"); }

@font-face { font-family:"JetBrains Mono"; font-style:normal; font-weight:400; font-display:swap;
  src:url("../fonts/jetbrains-mono-400.woff2") format("woff2"); }
@font-face { font-family:"JetBrains Mono"; font-style:normal; font-weight:500; font-display:swap;
  src:url("../fonts/jetbrains-mono-500.woff2") format("woff2"); }
```

- [ ] **Step 4: Write `site/css/styles.css` — tokens + base only (components added in Task 3)**

```css
/* ============ Design tokens ============ */
:root{
  --paper:#F4EFE3;
  --paper-2:#EDE7D9;
  --ink:#1F3DF5;
  --ink-soft:rgba(31,61,245,.5);
  --ink-hair:rgba(31,61,245,.22);
  --graphite:#2A2A28;
  --graphite-soft:#55524b;
  --accent:#E5392F;
  --ink-on-dark:#9FB4FF;
  --paper-on-dark:#ECE7D8;
  --term-bg:#0B1020;

  --display:"Anton",Impact,sans-serif;
  --sans:"Space Grotesk",system-ui,-apple-system,Segoe UI,Roboto,sans-serif;
  --mono:"JetBrains Mono",ui-monospace,SFMono-Regular,Menlo,monospace;

  --maxw:1120px;
  --pad:clamp(20px,5vw,64px);
  --section-y:clamp(64px,9vw,120px);
  --radius:8px;
}

/* ============ Base ============ */
*,*::before,*::after{box-sizing:border-box;}
html{scroll-behavior:smooth;}
body{
  margin:0; background:var(--paper); color:var(--graphite);
  font-family:var(--sans); font-weight:400; line-height:1.55;
  -webkit-font-smoothing:antialiased; text-rendering:optimizeLegibility;
}
img,svg{max-width:100%; height:auto; display:block;}
a{color:var(--ink); text-decoration:none;}
a:hover{text-decoration:underline;}
:focus-visible{outline:2px solid var(--ink); outline-offset:3px; border-radius:2px;}
h1,h2,h3,p{margin:0;}
```

- [ ] **Step 5: Write the `site/index.html` shell**

```html
<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>midnight-manual — a retrieval engine for the Midnight Network</title>
  <link rel="preload" href="fonts/anton-400.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="preload" href="fonts/space-grotesk-400.woff2" as="font" type="font/woff2" crossorigin>
  <link rel="stylesheet" href="css/fonts.css">
  <link rel="stylesheet" href="css/styles.css">
</head>
<body>
  <main id="top">
    <!-- sections added in later tasks -->
    <section class="section">
      <div class="container">
        <p class="eyebrow">// scaffold ok</p>
        <h1 class="display" style="font-size:48px;color:var(--ink)">Blueprint Editorial</h1>
      </div>
    </section>
  </main>
  <script type="module" src="js/main.js"></script>
</body>
</html>
```

- [ ] **Step 6: Create an empty JS module so the page doesn't 404**

```bash
printf '// main.js — populated in Task 3\n' > site/js/main.js
```

- [ ] **Step 7: Serve and verify the base renders**

```bash
cd site && python3 -m http.server 4173
```
Open `http://localhost:4173`. Expected: a cream (`#F4EFE3`) page, a blue mono eyebrow, and the heading "Blueprint Editorial" rendered in **Anton** (condensed, heavy). If the heading is in a serif/fallback, the font failed to load — recheck Step 2/3. Stop the server with Ctrl-C.

- [ ] **Step 8: Commit**

```bash
git add site/index.html site/css site/js site/fonts
git commit -m "feat(site): scaffold landing page shell, tokens, self-hosted fonts"
```

---

## Task 2: GitHub Pages deploy workflow + CNAME

**Files:**
- Create: `.github/workflows/pages.yml`, `site/CNAME`

- [ ] **Step 1: Write `site/CNAME`**

```text
manual.midnightntwrk.expert
```

(Single line, no trailing blank line beyond the newline.)

- [ ] **Step 2: Write `.github/workflows/pages.yml`**

```yaml
name: Deploy landing site

on:
  push:
    branches: [main]
    paths: ["site/**", ".github/workflows/pages.yml"]
  workflow_dispatch:

permissions:
  contents: read
  pages: write
  id-token: write

# Allow one concurrent deployment; don't cancel an in-progress run.
concurrency:
  group: pages
  cancel-in-progress: false

jobs:
  deploy:
    environment:
      name: github-pages
      url: ${{ steps.deployment.outputs.page_url }}
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/configure-pages@v5
      - uses: actions/upload-pages-artifact@v3
        with:
          path: site
      - id: deployment
        uses: actions/deploy-pages@v4
```

- [ ] **Step 3: Validate the workflow YAML**

```bash
python3 -c "import yaml,sys; yaml.safe_load(open('.github/workflows/pages.yml')); print('yaml ok')"
```
Expected: `yaml ok`. (If `actionlint` is installed, run it too; not required.)

- [ ] **Step 4: Confirm it doesn't disturb existing workflows**

```bash
ls .github/workflows
```
Expected: the new `pages.yml` alongside the existing `canary.yml ci.yml embedder-smoke.yml manifest-smoke.yml release.yml` — none modified.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/pages.yml site/CNAME
git commit -m "ci(site): GitHub Pages deploy workflow + CNAME (manual.midnightntwrk.expert)"
```

> **Manual, one-time (record in PR description, not a code step):** In repo Settings → Pages, set **Source = GitHub Actions**, and point DNS for `manual.midnightntwrk.expert` (CNAME → `devrelaicom.github.io`). Enable "Enforce HTTPS" once the cert provisions. These are outside this repo.

---

## Task 3: Shared components (CSS) + JS utilities

This task defines every reusable class and all JS behaviour. Later tasks only compose these.

**Files:**
- Modify: `site/css/styles.css` (append components), `site/js/main.js` (full content)

- [ ] **Step 1: Append the component layer to `site/css/styles.css`**

```css
/* ============ Layout ============ */
.container{max-width:var(--maxw); margin-inline:auto; padding-inline:var(--pad);}
.section{padding-block:var(--section-y);}
.section--band{background:var(--paper-2);}
.marker{font-family:var(--mono); font-size:12px; letter-spacing:.14em; text-transform:uppercase;
  color:var(--ink); opacity:.75; display:flex; align-items:center; gap:10px; margin-bottom:18px;}
.marker::before{content:""; width:26px; height:1px; background:var(--ink); opacity:.6;}

/* ============ Typography helpers ============ */
.eyebrow{font-family:var(--mono); font-weight:500; font-size:12px; letter-spacing:.14em;
  text-transform:uppercase; color:var(--ink); opacity:.8;}
.display{font-family:var(--display); font-weight:400; text-transform:uppercase;
  line-height:.9; letter-spacing:-.005em; color:var(--ink);}
.h-section{font-family:var(--display); font-weight:400; text-transform:uppercase;
  line-height:.92; letter-spacing:-.005em; color:var(--ink);
  font-size:clamp(32px,5vw,60px); margin-bottom:14px;}
.subhead{font-size:clamp(16px,2vw,19px); color:var(--graphite); max-width:60ch;}
.lead{font-size:clamp(18px,2.4vw,24px); color:var(--graphite); max-width:46ch; line-height:1.4;}
.mono{font-family:var(--mono); font-size:13px;}
.callout{font-family:var(--mono); font-size:12px; color:var(--ink);}

/* ============ Buttons ============ */
.btn-pill{display:inline-flex; align-items:center; gap:8px; font-family:var(--sans); font-weight:700;
  font-size:14px; color:var(--ink); border:1.5px solid var(--ink); border-radius:40px;
  padding:10px 24px; background:transparent; cursor:pointer; transition:background .15s,color .15s;}
.btn-pill:hover{background:var(--ink); color:var(--paper); text-decoration:none;}
.btn-pill--solid{background:var(--ink); color:var(--paper);}
.btn-pill--solid:hover{background:#152fd0;}

/* ============ Command strip + copy ============ */
.cmd{display:flex; align-items:center; justify-content:space-between; gap:12px;
  border:1px solid var(--ink); border-radius:var(--radius); padding:10px 12px;
  font-family:var(--mono); font-size:13px; color:var(--ink); background:transparent; overflow:auto;}
.cmd code{font-family:var(--mono); white-space:nowrap;}
.cmd__copy{flex:none; font-family:var(--mono); font-size:11px; letter-spacing:.08em; text-transform:uppercase;
  color:var(--ink); background:transparent; border:1px solid var(--ink-hair); border-radius:5px;
  padding:4px 8px; cursor:pointer;}
.cmd__copy:hover{background:var(--ink); color:var(--paper);}
.cmd__copy[data-copied="true"]{background:var(--accent); border-color:var(--accent); color:#fff;}

/* ============ Cards / grids ============ */
.cards{display:grid; gap:18px;}
.grid-2{grid-template-columns:repeat(2,1fr);}
.grid-3{grid-template-columns:repeat(3,1fr);}
.grid-4{grid-template-columns:repeat(4,1fr);}
.card{border:1px solid var(--ink-hair); border-radius:var(--radius); padding:22px; background:var(--paper);}
.card__title{font-family:var(--display); text-transform:uppercase; color:var(--ink);
  font-size:20px; line-height:1; margin-bottom:8px;}
.card__meta{font-family:var(--mono); font-size:11px; color:var(--ink); opacity:.7; margin-bottom:10px;}
.card p{font-size:14px; color:var(--graphite);}

/* ============ Terminal (the one dark panel — deliberate contrast) ============ */
.terminal{background:var(--term-bg); border-radius:var(--radius); overflow:hidden; font-family:var(--mono); font-size:13px;}
.terminal__bar{display:flex; gap:6px; padding:10px 12px; border-bottom:1px solid rgba(255,255,255,.08);}
.terminal__bar i{width:10px; height:10px; border-radius:50%; background:#33384d; display:inline-block;}
.terminal__body{padding:14px 16px; color:var(--ink-on-dark); line-height:1.7;}
.terminal__body .ok{color:#6ee7a8;}
.terminal__body .file{color:var(--paper-on-dark);}
.terminal__body .conf{color:#F2A33C;}

/* ============ Tabs ============ */
.tabs{display:flex; gap:4px; border:1px solid var(--ink-hair); border-radius:40px; padding:4px; width:max-content; max-width:100%; overflow:auto;}
.tab{font-family:var(--mono); font-size:12px; color:var(--ink); background:transparent; border:0;
  padding:7px 16px; border-radius:40px; cursor:pointer; white-space:nowrap;}
.tab[aria-selected="true"]{background:var(--ink); color:var(--paper);}
.tabpanel[hidden]{display:none;}

/* ============ Footer ============ */
.footer{border-top:1px solid var(--ink-hair); padding-block:48px; font-family:var(--mono); font-size:13px;}
.footer__links{display:flex; flex-wrap:wrap; gap:18px 28px; margin-top:14px;}

/* ============ Sticky install CTA ============ */
.sticky-cta{position:fixed; right:18px; bottom:18px; z-index:50; opacity:0; transform:translateY(12px);
  pointer-events:none; transition:opacity .25s, transform .25s;}
.sticky-cta.is-shown{opacity:1; transform:none; pointer-events:auto;}

/* ============ Reveal-on-scroll ============ */
[data-reveal]{opacity:0; transform:translateY(18px); transition:opacity .6s ease, transform .6s ease;}
[data-reveal].is-visible{opacity:1; transform:none;}

/* ============ Reduced motion ============ */
@media (prefers-reduced-motion: reduce){
  html{scroll-behavior:auto;}
  *{transition:none !important; animation:none !important;}
  [data-reveal]{opacity:1; transform:none;}
}

/* ============ Responsive ============ */
@media (max-width:820px){
  .grid-4{grid-template-columns:repeat(2,1fr);}
  .grid-3{grid-template-columns:1fr;}
  .grid-2{grid-template-columns:1fr;}
}
@media (max-width:520px){
  .grid-4{grid-template-columns:1fr;}
}
```

- [ ] **Step 2: Write the full `site/js/main.js`**

```js
// Progressive enhancement only — the page is fully usable without JS.
const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;

/* Copy buttons: <button class="cmd__copy" data-copy="text to copy">Copy</button> */
function initCopy(){
  document.querySelectorAll(".cmd__copy").forEach((btn) => {
    btn.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(btn.dataset.copy || "");
        const prev = btn.textContent;
        btn.dataset.copied = "true";
        btn.textContent = "Copied";
        setTimeout(() => { btn.dataset.copied = "false"; btn.textContent = prev; }, 1400);
      } catch { /* clipboard blocked; no-op */ }
    });
  });
}

/* Tabs: a .tabs with .tab[aria-controls=ID]; panels are .tabpanel[id=ID] */
function initTabs(){
  document.querySelectorAll(".tabs").forEach((group) => {
    const tabs = [...group.querySelectorAll(".tab")];
    const select = (tab) => {
      tabs.forEach((t) => {
        const on = t === tab;
        t.setAttribute("aria-selected", String(on));
        const panel = document.getElementById(t.getAttribute("aria-controls"));
        if (panel) panel.hidden = !on;
      });
    };
    tabs.forEach((t) => t.addEventListener("click", () => select(t)));
  });
}

/* Reveal on scroll */
function initReveals(){
  const els = document.querySelectorAll("[data-reveal]");
  if (reduceMotion || !("IntersectionObserver" in window)){
    els.forEach((el) => el.classList.add("is-visible"));
    return;
  }
  const io = new IntersectionObserver((entries) => {
    entries.forEach((e) => { if (e.isIntersecting){ e.target.classList.add("is-visible"); io.unobserve(e.target); } });
  }, { rootMargin: "0px 0px -10% 0px", threshold: 0.08 });
  els.forEach((el) => io.observe(el));
}

/* Engine assemble-on-load: toggles .engine--assembled (CSS drives the transition) */
function initEngine(){
  const engine = document.querySelector(".engine");
  if (!engine) return;
  if (reduceMotion){ engine.classList.add("engine--assembled"); return; }
  requestAnimationFrame(() => requestAnimationFrame(() => engine.classList.add("engine--assembled")));
}

/* Sticky CTA appears once the hero scrolls out of view */
function initStickyCta(){
  const cta = document.querySelector(".sticky-cta");
  const hero = document.getElementById("hero");
  if (!cta || !hero || !("IntersectionObserver" in window)) return;
  const io = new IntersectionObserver((entries) => {
    cta.classList.toggle("is-shown", !entries[0].isIntersecting);
  }, { threshold: 0 });
  io.observe(hero);
}

initCopy(); initTabs(); initReveals(); initEngine(); initStickyCta();
```

- [ ] **Step 3: Smoke-test the JS wiring with a temporary widget**

Temporarily replace the placeholder `<section>` in `index.html` `<main>` with:

```html
<section class="section"><div class="container">
  <div class="cmd"><code>echo hello</code>
    <button class="cmd__copy" data-copy="echo hello">Copy</button></div>
</div></section>
```

- [ ] **Step 4: Verify copy works**

```bash
cd site && python3 -m http.server 4173
```
Load `http://localhost:4173`, click **Copy** → label flips to "Copied" (red) then back. Paste into a text field to confirm `echo hello` was copied. Ctrl-C to stop.

- [ ] **Step 5: Commit**

```bash
git add site/css/styles.css site/js/main.js site/index.html
git commit -m "feat(site): shared component styles + JS (copy, tabs, reveals, engine, sticky CTA)"
```

---

## Task 4: Hero section (split layout + exploded engine SVG)

**Files:**
- Modify: `site/index.html` (replace the temporary `<main>` contents with the hero), `site/css/styles.css` (append hero + engine styles)

- [ ] **Step 1: Append hero + engine CSS to `site/css/styles.css`**

```css
/* ============ Hero ============ */
.hero{padding-block:clamp(36px,6vw,72px) var(--section-y);}
.hero__top{display:flex; justify-content:space-between; font-family:var(--mono); font-size:11px;
  letter-spacing:.12em; color:var(--ink); opacity:.75; border-bottom:1px solid var(--ink-hair);
  padding-bottom:14px; margin-bottom:36px;}
.hero__grid{display:grid; grid-template-columns:1.05fr .95fr; gap:48px; align-items:center;}
.hero__title{font-size:clamp(44px,7.5vw,104px); margin:0 0 18px;}
.hero__sub{font-size:clamp(16px,2vw,20px); color:var(--graphite); max-width:42ch; margin-bottom:26px;}
.hero__cmd{margin-bottom:22px; max-width:520px;}
.hero__cta{display:flex; gap:14px; flex-wrap:wrap;}

/* Engine: parts start nudged + faint, settle on .engine--assembled; leaders draw in */
.engine [data-part]{opacity:0; transform:translate(6px,8px); transition:opacity .55s ease, transform .55s ease;}
.engine [data-leader]{stroke-dasharray:240; stroke-dashoffset:240; transition:stroke-dashoffset .7s ease .35s;}
.engine [data-axis]{opacity:0; transition:opacity .5s ease;}
.engine--assembled [data-part]{opacity:1; transform:none;}
.engine--assembled [data-part]:nth-of-type(1){transition-delay:.05s;}
.engine--assembled [data-part]:nth-of-type(2){transition-delay:.13s;}
.engine--assembled [data-part]:nth-of-type(3){transition-delay:.21s;}
.engine--assembled [data-part]:nth-of-type(4){transition-delay:.29s;}
.engine--assembled [data-leader]{stroke-dashoffset:0;}
.engine--assembled [data-axis]{opacity:.4;}

@media (max-width:880px){
  .hero__grid{grid-template-columns:1fr; gap:32px;}
  .hero__title{font-size:clamp(40px,12vw,72px);}
}
```

- [ ] **Step 2: Replace the `<main>` body in `index.html` with the hero**

```html
<main id="top">
  <section class="hero section" id="hero">
    <div class="container">
      <div class="hero__top">
        <span>MCP · CLI · HTTP</span>
        <span>PRE-PRODUCTION · RUST 1.91</span>
      </div>
      <div class="hero__grid">
        <div>
          <p class="eyebrow" style="margin-bottom:14px">A retrieval engine for the Midnight Network</p>
          <h1 class="display hero__title">Ask your docs,<br>not your model</h1>
          <p class="hero__sub">Privacy-respecting hybrid search over the real Midnight docs and source — right inside your AI client.</p>
          <div class="cmd hero__cmd">
            <code>claude mcp add midnight-manual -- mnm mcp serve</code>
            <button class="cmd__copy" data-copy="claude mcp add midnight-manual -- mnm mcp serve">Copy</button>
          </div>
          <div class="hero__cta">
            <a class="btn-pill btn-pill--solid" href="#quickstart">Install</a>
            <a class="btn-pill" href="https://github.com/devrelaicom/midnight-manual" rel="noopener">View on GitHub ↗</a>
          </div>
        </div>
        <div class="engine" aria-hidden="true">
          <!-- Exploded "Retrieval Engine": discs along a diagonal axis with mono callouts -->
          <svg viewBox="0 0 460 360" width="100%" role="img" aria-label="Exploded diagram of the retrieval engine">
            <line data-axis x1="50" y1="300" x2="320" y2="70" stroke="var(--ink)" stroke-width="1" stroke-dasharray="3 5"/>
            <g fill="none" stroke="var(--ink)" stroke-width="1.6">
              <g data-part><ellipse cx="90" cy="270" rx="56" ry="18"/><ellipse cx="90" cy="262" rx="56" ry="18" opacity=".5"/></g>
              <g data-part><ellipse cx="165" cy="216" rx="56" ry="18"/><ellipse cx="165" cy="208" rx="56" ry="18" opacity=".5"/></g>
              <g data-part><ellipse cx="240" cy="162" rx="56" ry="18"/><ellipse cx="240" cy="154" rx="56" ry="18" opacity=".5"/></g>
              <g data-part><ellipse cx="315" cy="108" rx="56" ry="18"/><ellipse cx="315" cy="100" rx="56" ry="18" opacity=".5"/></g>
            </g>
            <g stroke="var(--ink)" stroke-width="1" opacity=".55">
              <line data-leader x1="146" y1="270" x2="350" y2="270"/>
              <line data-leader x1="221" y1="216" x2="350" y2="216"/>
              <line data-leader x1="296" y1="162" x2="350" y2="162"/>
              <line data-leader x1="371" y1="108" x2="380" y2="108"/>
            </g>
            <g font-family="var(--mono)" font-size="12" fill="var(--ink)">
              <text x="356" y="274">embed</text>
              <text x="356" y="220">hybrid · lexical+vector</text>
              <text x="356" y="166">RRF · k=60</text>
              <text x="386" y="112">rerank → confidence</text>
            </g>
          </svg>
        </div>
      </div>
    </div>
  </section>

  <!-- sticky install CTA -->
  <a class="btn-pill btn-pill--solid sticky-cta" href="#quickstart">Install</a>

  <!-- later sections appended below -->
</main>
```

- [ ] **Step 3: Verify the hero with the playwright-skill**

Serve (`cd site && python3 -m http.server 4173`), then invoke the **playwright-skill** to load `http://localhost:4173` and screenshot at width 1440 and width 390. Confirm: split layout (text left, engine right) on desktop; stacked on mobile; the engine's four discs and callouts render; the assemble animation plays once on load (parts fade/slide in, leader lines draw). Scroll down a little and confirm the floating **Install** pill appears.

- [ ] **Step 4: Commit**

```bash
git add site/index.html site/css/styles.css
git commit -m "feat(site): split hero with exploded retrieval-engine diagram"
```

---

## Task 5: Quick start section (steps + client tabs + terminal demo)

**Files:**
- Modify: `site/index.html` (append section after hero), `site/css/styles.css` (append a few quickstart rules)

- [ ] **Step 1: Append quickstart CSS**

```css
/* ============ Quick start ============ */
.qs-steps{display:grid; grid-template-columns:repeat(3,1fr); gap:20px; margin:26px 0 30px;}
.qs-step{border-top:2px solid var(--ink); padding-top:14px;}
.qs-step__n{font-family:var(--mono); font-size:11px; color:var(--ink); letter-spacing:.1em;}
.qs-step h3{font-family:var(--sans); font-weight:700; font-size:16px; margin:6px 0 6px;}
.qs-step p{font-size:14px; color:var(--graphite-soft);}
@media (max-width:820px){ .qs-steps{grid-template-columns:1fr;} }
```

- [ ] **Step 2: Append the Quick start section inside `<main>` (after the hero, before the sticky CTA comment is fine — append right after the hero `</section>`)**

```html
<section class="section section--band" id="quickstart">
  <div class="container" data-reveal>
    <p class="marker">Quick start</p>
    <h2 class="h-section">Up and running<br>in three steps</h2>
    <p class="subhead">No database, no API key, no account. You need a Rust toolchain (1.91+); the corpus and models are fetched on demand.</p>

    <div class="qs-steps">
      <div class="qs-step"><p class="qs-step__n">01</p><h3>Build the CLI</h3>
        <p>Clone, build the <code>mnm</code> binary, and put it on your PATH.</p></div>
      <div class="qs-step"><p class="qs-step__n">02</p><h3>Add to your client</h3>
        <p>One command wires the MCP server into your AI assistant.</p></div>
      <div class="qs-step"><p class="qs-step__n">03</p><h3>Search</h3>
        <p>Ask something Midnight-specific and get cited, ranked answers.</p></div>
    </div>

    <div class="cmd" style="margin-bottom:14px">
      <code>cargo build --release -p mn-cli &amp;&amp; install -m 0755 target/release/mnm ~/.local/bin/mnm</code>
      <button class="cmd__copy" data-copy="cargo build --release -p mn-cli && install -m 0755 target/release/mnm ~/.local/bin/mnm">Copy</button>
    </div>

    <div class="tabs" role="tablist" aria-label="Add to your client" style="margin-bottom:14px">
      <button class="tab" role="tab" aria-selected="true"  aria-controls="tab-claude">Claude Code</button>
      <button class="tab" role="tab" aria-selected="false" aria-controls="tab-codex">Codex</button>
      <button class="tab" role="tab" aria-selected="false" aria-controls="tab-cursor">Cursor</button>
    </div>
    <div id="tab-claude" class="tabpanel" role="tabpanel">
      <div class="cmd"><code>claude mcp add midnight-manual -- mnm mcp serve</code>
        <button class="cmd__copy" data-copy="claude mcp add midnight-manual -- mnm mcp serve">Copy</button></div>
    </div>
    <div id="tab-codex" class="tabpanel" role="tabpanel" hidden>
      <div class="cmd"><code># ~/.codex/config.toml — [mcp_servers.midnight-manual] command="mnm" args=["mcp","serve"]</code>
        <button class="cmd__copy" data-copy='[mcp_servers.midnight-manual]
command = "mnm"
args = ["mcp", "serve"]'>Copy</button></div>
    </div>
    <div id="tab-cursor" class="tabpanel" role="tabpanel" hidden>
      <div class="cmd"><code># ~/.cursor/mcp.json — { "mcpServers": { "midnight-manual": { "command": "mnm", "args": ["mcp","serve"] } } }</code>
        <button class="cmd__copy" data-copy='{ "mcpServers": { "midnight-manual": { "command": "mnm", "args": ["mcp", "serve"] } } }'>Copy</button></div>
    </div>

    <div class="terminal" style="margin-top:22px" data-reveal>
      <div class="terminal__bar"><i></i><i></i><i></i></div>
      <div class="terminal__body">
        <span class="ok">$</span> mnm search "how do I write a Compact contract with a sealed ledger?"<br>
        <span class="file">▸ midnight-docs › compact-ledger.md</span> &nbsp; <span class="conf">confidence 0.94</span> &nbsp; foundation · verified · recent<br>
        <span class="file">▸ openzeppelin-compact › access/Ownable.compact</span> &nbsp; <span class="conf">confidence 0.88</span> &nbsp; partner · version-match<br>
        <span class="file">▸ example-kitties › src/contract.compact</span> &nbsp; <span class="conf">confidence 0.81</span>
      </div>
    </div>
  </div>
</section>
```

- [ ] **Step 3: Verify tabs + terminal**

Serve and load. Click each client tab → only the matching command panel shows; selected tab is filled blue. Confirm the dark terminal panel renders with the orange confidence scores. Use the playwright-skill to screenshot if convenient.

- [ ] **Step 4: Commit**

```bash
git add site/index.html site/css/styles.css
git commit -m "feat(site): quick-start section with client tabs and search terminal demo"
```

---

## Task 6: The pitch band

**Files:**
- Modify: `site/index.html` (append after quickstart)

- [ ] **Step 1: Append the pitch section**

```html
<section class="section" id="why">
  <div class="container" data-reveal>
    <p class="lead">Your assistant's training data went stale the day it shipped.
    <strong>midnight-manual</strong> gives it hybrid search over the live Midnight corpus —
    ranked, reranked, and cited — with the embeddings running on your machine, so your queries stay yours.</p>
  </div>
</section>
```

- [ ] **Step 2: Verify** — serve, confirm the large `.lead` paragraph renders with bold product name and reveals on scroll.

- [ ] **Step 3: Commit**

```bash
git add site/index.html
git commit -m "feat(site): the pitch band"
```

---

## Task 7: The Retrieval Engine section

**Files:**
- Modify: `site/index.html` (append), `site/css/styles.css` (append a small trust-factor row style)

- [ ] **Step 1: Append CSS**

```css
.factors{display:grid; grid-template-columns:repeat(5,1fr); gap:12px; margin-top:24px;}
.factor{border:1px solid var(--ink-hair); border-radius:var(--radius); padding:14px;}
.factor b{font-family:var(--sans); font-weight:700; font-size:14px; display:block; margin-bottom:4px;}
.factor span{font-size:12px; color:var(--graphite-soft);}
@media (max-width:820px){ .factors{grid-template-columns:repeat(2,1fr);} }
```

- [ ] **Step 2: Append the section**

```html
<section class="section section--band" id="engine">
  <div class="container" data-reveal>
    <p class="marker">The retrieval engine</p>
    <h2 class="h-section">Confidence = trust × relevance</h2>
    <p class="subhead">Lexical (PostgreSQL full-text) and semantic (pgvector) candidates are fused with
    Reciprocal Rank Fusion (k=60). An optional <code>bge-reranker-base</code> cross-encoder sharpens the top
    results. Then every hit's relevance is multiplied by a <strong>trust score</strong> — and the per-factor
    breakdown comes back with it, so your assistant can explain <em>why</em> a passage is trustworthy.</p>
    <div class="factors">
      <div class="factor"><b>Attribution</b><span>Foundation › Partner › Third-party › Community.</span></div>
      <div class="factor"><b>Verification</b><span>Verified by the Foundation, a partner, or unverified.</span></div>
      <div class="factor"><b>Freshness</b><span>Exponential decay by age.</span></div>
      <div class="factor"><b>Deprecation</b><span>Flagged content is down-weighted.</span></div>
      <div class="factor"><b>Version match</b><span>Boosted when it targets your version.</span></div>
    </div>
  </div>
</section>
```

- [ ] **Step 3: Verify** — serve, confirm the five trust-factor cells render (5 across desktop, 2 across mobile).

- [ ] **Step 4: Commit**

```bash
git add site/index.html site/css/styles.css
git commit -m "feat(site): retrieval engine + trust factors section"
```

---

## Task 8: Three tools, one binary

**Files:**
- Modify: `site/index.html` (append)

- [ ] **Step 1: Append the section**

```html
<section class="section" id="tools">
  <div class="container" data-reveal>
    <p class="marker">Three tools, one binary</p>
    <h2 class="h-section">One workspace,<br>three ways in</h2>
    <div class="cards grid-3" style="margin-top:24px">
      <div class="card"><p class="card__meta">// MCP SERVER</p><p class="card__title">In your AI client</p>
        <p>Drop <code>mnm mcp serve</code> into Claude Code, Codex, or Cursor — your assistant gains hybrid search, reranking, and document navigation.</p></div>
      <div class="card"><p class="card__meta">// CLI · mnm</p><p class="card__title">In your terminal</p>
        <p>Search the corpus, inspect chunks and documents, and manage local models. Add <code>--json</code> to anything for scripting.</p></div>
      <div class="card"><p class="card__meta">// CLOUD CORPUS</p><p class="card__title">Hosted by default</p>
        <p>An <code>axum</code> + Postgres&nbsp;+&nbsp;pgvector service hosts the indexed corpus. It's the compiled-in default — most users never run a server.</p></div>
    </div>
  </div>
</section>
```

- [ ] **Step 2: Verify** — serve, confirm three blueprint cards (3-up desktop, 1-up mobile).

- [ ] **Step 3: Commit**

```bash
git add site/index.html
git commit -m "feat(site): three tools, one binary section"
```

---

## Task 9: In your AI client (the MCP server's 11 tools)

**Files:**
- Modify: `site/index.html` (append)

- [ ] **Step 1: Append the section** (tool groups: Search 1 + Read-in-context 7 + Corpus & models 3 = 11)

```html
<section class="section section--band" id="mcp">
  <div class="container" data-reveal>
    <p class="marker">In your AI client</p>
    <h2 class="h-section">11 tools, lazy-loaded</h2>
    <p class="subhead">A hand-rolled MCP server (JSON-RPC over stdio) that starts in under half a second and
    only loads the models the first time a query needs them. When the corpus rolls its embedding model forward,
    <code>search</code> returns a structured <code>embedding_model_mismatch</code> envelope that tells the client to <code>pull_models</code> — no cryptic failures.</p>
    <div class="cards grid-3" style="margin-top:24px">
      <div class="card"><p class="card__meta">// SEARCH</p><p class="card__title">Find</p>
        <p><code>search</code> — hybrid full-text + vector retrieval, optional rerank, filters by source / tier / language / package. Every hit carries a confidence score.</p></div>
      <div class="card"><p class="card__meta">// READ IN CONTEXT</p><p class="card__title">Navigate</p>
        <p><code>get_chunk</code>, <code>get_chunk_next/prev</code>, <code>get_chunk_parents</code>, <code>get_document</code>, <code>get_document_full</code>, <code>get_document_chunks</code> — pull exactly as much surrounding context as needed.</p></div>
      <div class="card"><p class="card__meta">// CORPUS &amp; MODELS</p><p class="card__title">Operate</p>
        <p><code>list_sources</code>, <code>pull_models</code>, <code>status</code> — enumerate sources, fetch the embedder + reranker on demand, and report health.</p></div>
    </div>
  </div>
</section>
```

- [ ] **Step 2: Verify** — serve; confirm the three tool-group cards and that the seven read-in-context tool names are present.

- [ ] **Step 3: Commit**

```bash
git add site/index.html
git commit -m "feat(site): MCP server 11-tools section"
```

---

## Task 10: It teaches the technique (Advanced Search Skill)

**Files:**
- Modify: `site/index.html` (append)

- [ ] **Step 1: Append the section**

```html
<section class="section" id="skill">
  <div class="container" data-reveal>
    <p class="marker">It teaches the technique</p>
    <h2 class="h-section">Searches like a<br>seasoned researcher</h2>
    <p class="subhead">The server gives your assistant the power tools; the Advanced Search Skill teaches the
    technique. It's a persistent, auto-loaded Agent Skill — install once and your agent reaches for the right
    retrieval pattern on its own.</p>
    <div class="cards grid-4" style="margin:24px 0">
      <div class="card"><p class="card__title">HyDE</p><p>Draft a hypothetical answer and search with it alongside the question.</p></div>
      <div class="card"><p class="card__title">Multi-query</p><p>Fuse 2–3 paraphrases to beat synonym mismatch.</p></div>
      <div class="card"><p class="card__title">Step-back</p><p>Pair the specific question with a broader framing.</p></div>
      <div class="card"><p class="card__title">Lexical anchoring</p><p>Send exact identifiers and error codes verbatim.</p></div>
      <div class="card"><p class="card__title">Symbol-aware</p><p>Scope by package / language, navigate by symbol path.</p></div>
      <div class="card"><p class="card__title">Retrieve-read-retrieve</p><p>Broad pass → read neighbours → refine → search again.</p></div>
      <div class="card"><p class="card__title">Trust-weighted</p><p>Rank and prune on each result's trust factors.</p></div>
      <div class="card"><p class="card__title">Cross-source</p><p>Surface disagreement instead of silently picking one.</p></div>
    </div>
    <div class="cmd" style="max-width:420px">
      <code>mnm skills add</code>
      <button class="cmd__copy" data-copy="mnm skills add">Copy</button>
    </div>
  </div>
</section>
```

- [ ] **Step 2: Verify** — serve; confirm 8 technique cards (4-up desktop, 2-up tablet, 1-up phone) and the install command.

- [ ] **Step 3: Commit**

```bash
git add site/index.html
git commit -m "feat(site): advanced search skill techniques grid"
```

---

## Task 11: The smart chunker

**Files:**
- Modify: `site/index.html` (append), `site/css/styles.css` (append language-grid styles)

- [ ] **Step 1: Append CSS**

```css
.langs{display:flex; flex-wrap:wrap; gap:8px; margin-top:22px;}
.lang{font-family:var(--mono); font-size:12px; color:var(--ink); border:1px solid var(--ink-hair);
  border-radius:5px; padding:6px 10px;}
.lang--first{border-color:var(--accent); color:var(--accent);}
```

- [ ] **Step 2: Append the section**

```html
<section class="section section--band" id="chunker">
  <div class="container" data-reveal>
    <p class="marker">The smart chunker</p>
    <h2 class="h-section">It understands structure</h2>
    <p class="subhead">Markdown is split along its heading hierarchy (every chunk carries its
    <code>heading_path</code>). Source files are parsed with tree-sitter and split on real syntactic
    boundaries — functions, impl blocks, modules — never mid-expression, each recording a structured
    <code>symbol_path</code>. <strong>Compact is a first-class citizen:</strong> circuits, ledger
    declarations, witnesses, and contracts each become their own attributable chunk.</p>
    <div class="langs">
      <span class="lang lang--first">Compact</span>
      <span class="lang">Rust</span><span class="lang">TypeScript</span><span class="lang">JavaScript</span>
      <span class="lang">Python</span><span class="lang">Go</span><span class="lang">Solidity</span>
      <span class="lang">Java</span><span class="lang">C#</span><span class="lang">Kotlin</span>
      <span class="lang">Swift</span><span class="lang">Ruby</span><span class="lang">Haskell</span>
      <span class="lang">Bash</span><span class="lang">Scheme</span><span class="lang">TOML</span>
      <span class="lang">YAML</span><span class="lang">HTML / XML</span>
    </div>
  </div>
</section>
```

- [ ] **Step 3: Verify** — serve; confirm the language pills wrap, with **Compact** highlighted in red.

- [ ] **Step 4: Commit**

```bash
git add site/index.html site/css/styles.css
git commit -m "feat(site): smart chunker section + language grid"
```

---

## Task 12: Local & private

**Files:**
- Modify: `site/index.html` (append)

- [ ] **Step 1: Append the section**

```html
<section class="section" id="private">
  <div class="container" data-reveal>
    <p class="marker">Local &amp; private</p>
    <h2 class="h-section">Your queries<br>stay yours</h2>
    <div class="cards grid-3" style="margin-top:24px">
      <div class="card"><p class="card__title">Local models</p>
        <p>Embedding (<code>bge-base-en-v1.5</code>) and reranking (<code>bge-reranker-base</code>) run on your machine via fastembed/ONNX — no Python, no GPU, no API key, no account.</p></div>
      <div class="card"><p class="card__title">Opt-out telemetry</p>
        <p>Carries no query or chunk content, no tokens, no paths. Opt out three ways: an env var, a config flag, or <code>mnm telemetry disable</code>.</p></div>
      <div class="card"><p class="card__title">Enforced by a canary</p>
        <p>A CI test feeds fake tokens, paths, and queries through every path that touches user content — any leak fails the build.</p></div>
    </div>
  </div>
</section>
```

- [ ] **Step 2: Verify** — serve; confirm three cards render.

- [ ] **Step 3: Commit**

```bash
git add site/index.html
git commit -m "feat(site): local & private section"
```

---

## Task 13: Footer + SEO/meta + favicon + OG image

**Files:**
- Modify: `site/index.html` (append footer; expand `<head>`), Create: `site/img/favicon.svg`, `site/img/og.png`

- [ ] **Step 1: Append the footer inside `<main>` (last child) and close tags**

```html
<footer class="footer">
  <div class="container">
    <p>Pre-production software for the Midnight Network. The hosted corpus resets without notice and interfaces move. <strong>Build something anyway.</strong></p>
    <nav class="footer__links">
      <a href="https://github.com/devrelaicom/midnight-manual" rel="noopener">GitHub ↗</a>
      <a href="https://github.com/devrelaicom/midnight-manual/blob/main/specs/001-rag-platform/spec.md" rel="noopener">Spec</a>
      <a href="https://github.com/devrelaicom/midnight-manual/blob/main/CONSTITUTION.md" rel="noopener">Constitution</a>
      <a href="https://github.com/devrelaicom/midnight-manual/blob/main/docs/cookbook/query-enhancement.md" rel="noopener">Cookbook</a>
    </nav>
  </div>
</footer>
```

- [ ] **Step 2: Create `site/img/favicon.svg`** (hand-authored blueprint mark — exploded discs glyph)

```svg
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32">
  <rect width="32" height="32" fill="#F4EFE3"/>
  <g fill="none" stroke="#1F3DF5" stroke-width="1.6">
    <ellipse cx="11" cy="22" rx="7" ry="2.6"/>
    <ellipse cx="16" cy="16" rx="7" ry="2.6"/>
    <ellipse cx="21" cy="10" rx="7" ry="2.6"/>
  </g>
</svg>
```

- [ ] **Step 3: Generate the OG share image with the nanobanana skill**

Invoke the **image-gen:nanobanana** skill with this prompt, save output to `site/img/og.png`, dimensions **1200×630**:

> "A technical blueprint-style poster on a warm cream paper background (#F4EFE3). Centered: a thin electric-blue (#1F3DF5) line-art exploded mechanical diagram of stacked discs along a diagonal axis, with small monospace callout labels. Bold uppercase headline 'ASK YOUR DOCS, NOT YOUR MODEL' in electric blue. Small monospace tagline 'midnight-manual · a retrieval engine for the Midnight Network'. A single tiny red (#E5392F) registration tick. Editorial, minimal, lots of negative space. No photorealism, no gradients."

If generation isn't possible in this run, create a temporary `site/img/og.png` by screenshotting the hero at 1200×630 with the playwright-skill, and note in the PR that the final OG art is pending.

- [ ] **Step 4: Expand the `<head>` — insert these lines after the existing `<title>`**

```html
  <meta name="description" content="A privacy-respecting retrieval engine for the Midnight Network — so your AI assistant answers from the real docs and source, not a stale training set.">
  <link rel="canonical" href="https://manual.midnightntwrk.expert/">
  <link rel="icon" type="image/svg+xml" href="img/favicon.svg">
  <meta property="og:type" content="website">
  <meta property="og:title" content="midnight-manual — ask your docs, not your model">
  <meta property="og:description" content="Privacy-respecting hybrid search over the real Midnight docs and source, right inside your AI client.">
  <meta property="og:url" content="https://manual.midnightntwrk.expert/">
  <meta property="og:image" content="https://manual.midnightntwrk.expert/img/og.png">
  <meta name="twitter:card" content="summary_large_image">
  <meta name="twitter:title" content="midnight-manual — ask your docs, not your model">
  <meta name="twitter:description" content="Privacy-respecting hybrid search over the real Midnight docs and source.">
  <meta name="twitter:image" content="https://manual.midnightntwrk.expert/img/og.png">
```

- [ ] **Step 5: Verify** — serve; confirm the favicon shows in the browser tab, the footer renders with all four links, and `view-source` shows the OG/Twitter tags. Confirm `site/img/og.png` exists and is 1200×630 (`python3 -c "from struct import unpack;d=open('site/img/og.png','rb').read(24);print(unpack('>II',d[16:24]))"` → `(1200, 630)`).

- [ ] **Step 6: Commit**

```bash
git add site/index.html site/img/favicon.svg site/img/og.png
git commit -m "feat(site): footer, SEO/OG meta, favicon, OG share image"
```

---

## Task 14: Polish pass — responsive, a11y, performance

**Files:**
- Modify: `site/css/styles.css` (small fixes as found), `site/index.html` (a11y attributes as found)

- [ ] **Step 1: Full-page screenshot sweep with the playwright-skill**

Serve, then use the playwright-skill to load `http://localhost:4173` and capture full-page screenshots at widths **1440, 768, 390**. Visually confirm: no horizontal overflow, hero stacks cleanly on mobile, all grids reflow, the terminal panel doesn't overflow, section bands alternate correctly.

- [ ] **Step 2: Reduced-motion check**

In the playwright-skill, emulate `prefers-reduced-motion: reduce` and reload. Confirm all `[data-reveal]` content is visible immediately (no hidden sections) and the engine shows assembled with no animation.

- [ ] **Step 3: Behaviour checks**

With the playwright-skill: (a) click a Quick-start client tab and assert only its panel is visible; (b) click a copy button and assert its label becomes "Copied"; (c) tab through the page and confirm visible focus rings on links/buttons.

- [ ] **Step 4: Contrast + semantics audit**

Confirm body text uses `--graphite` on `--paper` (passes AA at body sizes) and blue-on-cream is only used at large/bold sizes or for non-text. Confirm exactly one `<h1>` (hero), section headings are `<h2>`, the engine SVG has `aria-label` and the decorative wrapper is `aria-hidden`. Fix any violation inline.

- [ ] **Step 5: Performance sanity**

Confirm: no third-party network requests at runtime (fonts are local); `preload` hints present for the two above-the-fold fonts; SVG used for all line-art; `og.png` is the only sizeable raster. If Lighthouse is available, run it and aim for Performance/Best-Practices/SEO ≥ 95; otherwise note skipped.

- [ ] **Step 6: Commit any fixes**

```bash
git add -A site
git commit -m "fix(site): responsive, a11y, and performance polish"
```

---

## Self-Review (completed by plan author)

**Spec coverage:** Visual system → Tasks 1,3,4. Color tokens/type → Task 1. Motif/SVG + nanobanana split → Tasks 4,13. Motion + reduced-motion → Tasks 3,4,14. All 10 page sections → Tasks 4–13. Copy (§4) → embedded verbatim in Tasks 4–13. Quick start high on page → Task 5 (immediately after hero). Out-of-scope sections → omitted (not built). Tech/static `site/` + relative paths → Task 1. CNAME + workflow → Task 2. Responsive/a11y/perf/SEO → Tasks 13,14. Acceptance criteria 1–8 → all mapped. Decisions log → no code. Open items: brand-blue (token in Task 1, easy swap), OG image (Task 13), sticky mini-nav → resolved as a minimal sticky **Install** pill only (full nav dropped per YAGNI; documented here).

**Placeholder scan:** No "TBD/TODO". Font filenames are made deterministic by the rename step. The one external artifact (OG image) has a concrete generation step plus a defined fallback.

**Type/identifier consistency:** Class names match the Task 3 vocabulary throughout. JS hooks (`.cmd__copy[data-copy]`, `.tab[aria-controls]`+`.tabpanel#id`, `[data-reveal].is-visible`, `.engine`+`.engine--assembled`+`[data-part]/[data-leader]/[data-axis]`, `#hero`+`.sticky-cta.is-shown`) are defined in Task 3 and used consistently in Tasks 4–5. Section `id`s (`#quickstart`) match the hero/sticky CTA anchors.

---

## Notes for the implementer
- Keep all asset paths **relative** (no leading `/`) so the page works under the custom domain or any base path.
- The README is the source of truth for every product claim; do not add features the README doesn't describe.
- The only intentionally dark element is the terminal panel in Task 5 — everything else stays on cream.
