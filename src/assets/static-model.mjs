export function readState(params, options) {
  const allowed = new Set(["period", "authors", "author", "source"]);
  for (const key of params.keys()) {
    if (!allowed.has(key)) throw new Error(`Unsupported filter parameter: ${key}`);
  }
  for (const key of ["period", "authors"]) {
    if (params.getAll(key).length > 1) throw new Error(`Repeated filter parameter: ${key}`);
  }
  const period = params.get("period") || "";
  const mode = params.get("authors") || "default";
  if (!["", "30d", "90d", "1y"].includes(period)) throw new Error("Unknown date period.");
  if (!["default", "all", "custom"].includes(mode)) throw new Error("Unknown author selection.");
  const authors = [...new Set(params.getAll("author"))];
  const sources = params.has("source") ? [...new Set(params.getAll("source"))] : [...options.sources];
  if (authors.some((id) => !options.authors.includes(id))) throw new Error("Unknown author in this snapshot.");
  if (sources.some((id) => !options.sources.includes(id))) throw new Error("Unknown source in this snapshot.");
  return { period, mode, authors, sources };
}

export function writeState(state, options) {
  const params = new URLSearchParams();
  if (state.period) params.set("period", state.period);
  if (state.mode !== "default") params.set("authors", state.mode);
  if (state.mode === "custom") {
    for (const id of [...state.authors].sort()) params.append("author", id);
  }
  if (state.sources.length !== options.sources.length) {
    for (const id of [...state.sources].sort()) params.append("source", id);
  }
  return params;
}

export function matches(article, state, now = new Date()) {
  const days = { "30d": 30, "90d": 90, "1y": 365 }[state.period];
  if (days) {
    const cutoff = Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate()) - days * 86400000;
    const date = /^\d{4}-\d{2}-\d{2}$/.test(article.date) ? Date.parse(`${article.date}T00:00:00Z`) : NaN;
    if (!(date >= cutoff)) return false;
  }
  const authorMatches = state.mode === "all" || (state.mode === "default"
    ? article.defaultAuthor
    : article.authors.some((author) => state.authors.includes(author)));
  const sourceMatches = state.sources.length === 0 || article.sources.some((source) => state.sources.includes(source));
  return authorMatches && sourceMatches;
}

export function safeReturn(value, root, origin) {
  if (!value || !value.startsWith("/") || value.startsWith("//")) return null;
  const url = new URL(value, origin);
  if (url.origin !== origin || !url.pathname.startsWith(root)) return null;
  const relative = url.pathname.slice(root.length);
  if (relative !== "" && !/^feeds\/[a-z0-9][a-z0-9-]*\/$/.test(relative)) return null;
  return `${url.pathname}${url.search}`;
}
