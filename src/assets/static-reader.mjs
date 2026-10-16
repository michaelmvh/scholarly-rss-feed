import { matches, readState, safeReturn, writeState } from "./static-model.mjs";

const main = document.querySelector("[data-static-root]");
const back = document.querySelector("a.back");
if (main && back) {
  const target = safeReturn(new URLSearchParams(location.search).get("return"), main.dataset.staticRoot, location.origin);
  if (target) back.href = target;
}

const form = document.querySelector("[data-static-filters]");
if (form) {
  const authorBoxes = [...form.querySelectorAll('[name="author"]')];
  const sourceBoxes = [...form.querySelectorAll('[name="source"]')];
  const mode = form.querySelector('[name="authors"]');
  const period = form.querySelector('[name="period"]');
  const options = { authors: authorBoxes.map((box) => box.value), sources: sourceBoxes.map((box) => box.value) };
  const cards = [...document.querySelectorAll("[data-article]")].map((element) => ({
    element,
    date: element.dataset.date,
    defaultAuthor: element.dataset.defaultAuthor === "true",
    authors: JSON.parse(element.dataset.authors),
    sources: JSON.parse(element.dataset.sources),
  }));
  const error = document.querySelector("[data-filter-error]");
  const count = document.querySelector("[data-filter-count]");
  const empty = document.querySelector("[data-filter-empty]");
  const showError = (problem) => {
    error.textContent = `${problem.message} Clear filters to restore the default view.`;
    error.hidden = false;
  };
  const sync = (state) => {
    mode.value = state.mode;
    period.value = state.period;
    for (const box of authorBoxes) {
      box.checked = state.mode === "all" || (state.mode === "default" ? box.dataset.default === "true" : state.authors.includes(box.value));
    }
    for (const box of sourceBoxes) box.checked = state.sources.includes(box.value);
  };
  const display = (state, push) => {
    const query = writeState(state, options).toString();
    const returnPath = `${location.pathname}${query ? `?${query}` : ""}`;
    if (push && returnPath !== `${location.pathname}${location.search}`) history.pushState(null, "", returnPath);
    let visible = 0;
    for (const card of cards) {
      card.element.hidden = !matches(card, state);
      if (!card.element.hidden) visible += 1;
      const link = card.element.querySelector("h2 a");
      const url = new URL(link.href);
      url.search = new URLSearchParams({ return: returnPath }).toString();
      link.href = url.href;
    }
    count.textContent = `${visible} of ${cards.length} publications`;
    empty.hidden = visible !== 0;
    error.hidden = true;
  };
  const restore = () => {
    try {
      const state = readState(new URLSearchParams(location.search), options);
      sync(state);
      display(state, false);
    } catch (problem) {
      showError(problem);
    }
  };
  const apply = () => {
    const state = {
      period: period.value, mode: mode.value,
      authors: authorBoxes.filter((box) => box.checked).map((box) => box.value),
      sources: sourceBoxes.filter((box) => box.checked).map((box) => box.value),
    };
    if (sourceBoxes.length && !state.sources.length) {
      showError(new Error("Select at least one paper source."));
      return;
    }
    display(state, true);
  };
  form.addEventListener("submit", (event) => { event.preventDefault(); apply(); });
  form.addEventListener("change", (event) => {
    if (event.target.name === "author") mode.value = authorBoxes.every((box) => box.checked) ? "all" : "custom";
    if (event.target === mode) {
      for (const box of authorBoxes) {
        if (mode.value !== "custom") box.checked = mode.value === "all" || box.dataset.default === "true";
      }
    }
    apply();
  });
  form.addEventListener("reset", (event) => {
    event.preventDefault();
    const state = readState(new URLSearchParams(), options);
    sync(state);
    display(state, true);
  });
  window.addEventListener("popstate", restore);
  form.hidden = false;
  document.querySelector("[data-filter-notice]").hidden = true;
  restore();
}
