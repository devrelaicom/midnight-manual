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
        setTimeout(() => { btn.removeAttribute("data-copied"); btn.textContent = prev; }, 1400);
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
    // Keyboard support for the ARIA tablist pattern (arrow/Home/End).
    group.addEventListener("keydown", (e) => {
      const i = tabs.indexOf(document.activeElement);
      if (i === -1) return;
      let n = null;
      if (e.key === "ArrowRight") n = (i + 1) % tabs.length;
      else if (e.key === "ArrowLeft") n = (i - 1 + tabs.length) % tabs.length;
      else if (e.key === "Home") n = 0;
      else if (e.key === "End") n = tabs.length - 1;
      if (n !== null){ e.preventDefault(); tabs[n].focus(); select(tabs[n]); }
    });
    // Enforce initial state from JS so sections need not pre-hide panels.
    if (tabs.length) select(tabs[0]);
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
