//! `--html`'s self-contained interactive visual report -- a single HTML
//! file with the whole plan embedded as JSON, rendered and made
//! interactive entirely client-side (search, upstream/downstream
//! highlighting on selection, a resizable side panel). No network/CDN
//! reference anywhere in the output, so it works fully offline; nothing
//! here is generated unless `--html` is actually passed (§ "Part 2" of
//! issue #57 -- opt-in only, most runs don't need it).
//!
//! Visual/interaction precedent is `zhao-cli`'s own `zhao lineage --html`
//! (`crates/zhao-cli/src/lineage_html.rs`) -- read for its structure and
//! CSS/JS conventions, not imported: this addon has no technical
//! dependency on `zhao-cli`/`zhao-core` at all (see ADR 0010), same as
//! `git.rs`/`state.rs` already independently mirror `zhao-core`'s own
//! git-native logic. One deliberate departure from that precedent: node
//! layout here is plain HTML/CSS (flex columns of divs), not SVG text --
//! a model name can be arbitrarily long, and CSS wrapping a `<div>` needs
//! no text-measurement trick to never clip it, unlike sizing an SVG
//! `<rect>` around a `<text>` element. `lineage_html.rs`'s panel truncates
//! long column expressions with `text-overflow: ellipsis`
//! (`lineage_html.rs:453`); this report never does that for model names
//! anywhere, which is why the layout choice differs.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::output::now_compact_utc_timestamp;
use crate::plan::Plan;

/// One model, reshaped for the embedded JSON -- everything the JSON
/// report already carries that's relevant to a visual read of the DAG's
/// structure (full name, window, layer); intentionally not the whole
/// `ModelDocument` shape (no `lookback`/`lookahead` raw config), since
/// this report is about the cascading structure, not re-deriving the
/// plan JSON in a browser.
#[derive(Debug, Serialize)]
struct GraphNode {
    name: String,
    layer: usize,
    event_time_start: String,
    event_time_end: String,
    depends_on: Vec<String>,
}

/// A single upstream -> downstream edge, derived from every model's own
/// `depends_on` (already within-selection only -- see `plan.rs`).
#[derive(Debug, Serialize)]
struct GraphEdge {
    upstream: String,
    downstream: String,
}

/// Everything embedded into the page as `window.ZHAO_DBT_PLAN_DATA`.
#[derive(Debug, Serialize)]
struct GraphData {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

/// The default destination for `--html`'s output: a directory distinct
/// from wherever the JSON's `--output-file` points (§ "Part 2": "a NEW
/// directory `target/zhao/dbt-plan/`... distinct from wherever
/// `--output-file`'s JSON goes"), named with a compact UTC timestamp so
/// repeat runs in a disposable CI environment never collide and nothing
/// needs cleaning up.
pub fn default_output_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join("target")
        .join("zhao")
        .join("dbt-plan")
        .join(format!("dbt_plan_{}.html", now_compact_utc_timestamp()))
}

/// Builds the whole HTML document for `built_plan`.
pub fn generate(built_plan: &Plan) -> String {
    let nodes = built_plan
        .models
        .iter()
        .map(|m| GraphNode {
            name: m.name.clone(),
            layer: m.layer,
            event_time_start: m.window.start.to_string(),
            event_time_end: m.window.end.to_string(),
            depends_on: m.depends_on.clone(),
        })
        .collect();

    let edges = built_plan
        .models
        .iter()
        .flat_map(|m| {
            m.depends_on.iter().map(move |upstream| GraphEdge {
                upstream: upstream.clone(),
                downstream: m.name.clone(),
            })
        })
        .collect();

    let data = GraphData { nodes, edges };
    let json = serde_json::to_string(&data).expect("graph data should always serialize");

    // The same default-yesterday note shown unconditionally on stderr
    // (`main.rs`) and in the JSON's `metadata.anchor_window.note`
    // (`output.rs`) -- rendered here as a visible header banner rather
    // than requiring the reader to already know to look for it. Absent
    // entirely (no empty banner element left behind) when the plan used
    // an explicit window -- see `Plan::default_yesterday_note`.
    let banner_html = match built_plan.default_yesterday_note() {
        Some(note) => format!(
            r#"<div id="default-yesterday-banner">{}</div>"#,
            escape_html(&note)
        ),
        None => String::new(),
    };

    render_html(&json, &banner_html)
}

/// Escapes the handful of characters that matter inside an HTML text
/// node -- just enough for the plain, punctuation-only note text this is
/// ever used on (see [`generate`]'s `banner_html`), not a general HTML
/// escaper.
fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Wraps the embedded JSON in the full HTML/CSS/JS document. Every other
/// byte of markup/style/script is a plain string literal -- easy to audit
/// for "no external references anywhere." `banner_html` is the
/// default-yesterday note banner (already-rendered markup, or empty when
/// there's nothing to show -- see [`generate`]).
fn render_html(graph_data_json: &str, banner_html: &str) -> String {
    format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>zhao-dbt-plan</title>
<style>
{CSS}
</style>
</head>
<body>
<div class="viz-root">
  {banner_html}
  <header id="toolbar">
    <div class="brand">zhao <span class="brand-sub">dbt-plan</span></div>
    <div class="search-wrap">
      <svg class="search-icon" viewBox="0 0 16 16" width="14" height="14"><path d="M11.2 9.8h-.6l-.2-.2c.8-.9 1.3-2.1 1.3-3.4C11.7 3.3 9.4 1 6.6 1S1.5 3.3 1.5 6.1s2.3 5.1 5.1 5.1c1.3 0 2.5-.5 3.4-1.3l.2.2v.6l3.4 3.4 1-1-3.4-3.3zM6.6 9.8c-2 0-3.7-1.7-3.7-3.7S4.6 2.4 6.6 2.4s3.7 1.7 3.7 3.7-1.7 3.7-3.7 3.7z" fill="currentColor"/></svg>
      <input id="search" type="text" placeholder="Search models…" autocomplete="off">
    </div>
    <div class="legend">
      <span class="legend-item"><span class="legend-dot"></span>model</span>
    </div>
  </header>
  <div id="main">
    <div id="graph-scroll">
      <svg id="edges"></svg>
      <div id="layers"></div>
    </div>
    <div id="panel-resize-handle" title="Drag to resize"></div>
    <aside id="panel">
      <div id="panel-empty">Select a model to inspect its window and lineage.</div>
      <div id="panel-content">
        <div class="panel-kind">model</div>
        <h2 id="panel-title"></h2>
        <div id="panel-layer"></div>
        <div id="panel-window"></div>
        <div class="panel-section-label">Upstream</div>
        <ul id="panel-upstream"></ul>
        <div class="panel-section-label">Downstream</div>
        <ul id="panel-downstream"></ul>
      </div>
    </aside>
  </div>
</div>
<script>
window.ZHAO_DBT_PLAN_DATA = {graph_data_json};
{JS}
</script>
</body>
</html>
"##,
        CSS = CSS,
        JS = JS,
        graph_data_json = graph_data_json,
        banner_html = banner_html,
    )
}

/// Same palette/chrome-role convention `lineage_html.rs` follows (see its
/// own doc comment on its `CSS` constant) -- one categorical series slot
/// (blue, "model") since this report has only one kind of node, unlike
/// lineage's Node/Origin distinction.
const CSS: &str = r#"
:root { color-scheme: light dark; }
* { box-sizing: border-box; }

.viz-root {
  --surface-1:      #fcfcfb;
  --surface-2:      #f9f9f7;
  --text-primary:   #0b0b0b;
  --text-secondary: #52514e;
  --text-muted:     #898781;
  --gridline:       #e1e0d9;
  --border:         rgba(11,11,11,0.10);
  --series-model:   #2a78d6;
  --series-model-soft: #2a78d61a;
  color-scheme: light;
}
@media (prefers-color-scheme: dark) {
  .viz-root {
    --surface-1:      #1a1a19;
    --surface-2:      #0d0d0d;
    --text-primary:   #ffffff;
    --text-secondary: #c3c2b7;
    --text-muted:     #898781;
    --gridline:       #2c2c2a;
    --border:         rgba(255,255,255,0.10);
    --series-model:   #3987e5;
    --series-model-soft: #3987e526;
    color-scheme: dark;
  }
}

body {
  margin: 0; font-family: system-ui, -apple-system, "Segoe UI", sans-serif;
  background: radial-gradient(120% 100% at 0% 0%, var(--series-model-soft), transparent 55%),
              var(--surface-2);
}
.viz-root { display: flex; flex-direction: column; height: 100vh; color: var(--text-primary); }

/* The default-yesterday note, when the plan used it (see
   `generate`/`Plan::default_yesterday_note`) -- a visible banner above
   the toolbar, not buried, so a reader never has to already know to look
   for it. Absent from the DOM entirely (not just hidden) when the plan
   used an explicit window. */
#default-yesterday-banner {
  padding: 8px 20px; font-size: 12.5px; background: var(--series-model-soft);
  color: var(--text-primary); border-bottom: 1px solid var(--border);
}

#toolbar {
  display: flex; align-items: center; gap: 20px;
  padding: 12px 20px; background: var(--surface-1); border-bottom: 1px solid var(--border);
}
.brand { font-weight: 600; font-size: 15px; letter-spacing: -0.01em; white-space: nowrap; }
.brand-sub { font-weight: 400; color: var(--text-muted); }

.search-wrap { position: relative; flex: 1; max-width: 360px; }
.search-icon { position: absolute; left: 10px; top: 50%; transform: translateY(-50%); color: var(--text-muted); pointer-events: none; }
#search {
  width: 100%; padding: 7px 12px 7px 30px; font-size: 13px; border-radius: 8px;
  border: 1px solid var(--border); background: var(--surface-2); color: var(--text-primary);
  outline: none; transition: border-color 0.15s ease, box-shadow 0.15s ease;
}
#search:focus { border-color: var(--series-model); box-shadow: 0 0 0 3px var(--series-model-soft); }

.legend { display: flex; gap: 14px; margin-left: auto; }
.legend-item { display: flex; align-items: center; gap: 6px; font-size: 12px; color: var(--text-secondary); white-space: nowrap; }
.legend-dot { width: 8px; height: 8px; border-radius: 50%; background: var(--series-model); }

#main { flex: 1; display: flex; overflow: hidden; }
#graph-scroll { flex: 1; overflow: auto; position: relative; }
#edges { position: absolute; top: 0; left: 0; pointer-events: none; }
#layers { display: flex; align-items: flex-start; gap: 64px; padding: 32px; position: relative; }
.layer-col { display: flex; flex-direction: column; gap: 20px; }

/* Drag handle between the graph and the panel, same convention
   `lineage_html.rs`'s own `#panel-resize-handle` uses. */
#panel-resize-handle {
  width: 6px; flex-shrink: 0; cursor: col-resize; background: var(--border);
  position: relative;
}
#panel-resize-handle:hover, #panel-resize-handle.resizing { background: var(--series-model); }

#panel {
  width: 340px; min-width: 240px; flex-shrink: 0; border-left: 1px solid var(--border);
  background: var(--surface-1); padding: 20px; overflow: auto;
}
#panel-empty { color: var(--text-muted); font-size: 13px; line-height: 1.5; }
#panel-content { display: none; }
.panel-kind { font-size: 11px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-muted); margin-bottom: 2px; }
/* Hard requirement (issue #57): a model name must NEVER truncate,
   regardless of length. No `overflow: hidden`, no `text-overflow`, no
   `white-space: nowrap` on this element or `.node-name` below -- long
   names wrap onto further lines instead. */
#panel-title {
  font-size: 16px; font-weight: 600; margin: 0 0 12px; letter-spacing: -0.01em;
  white-space: normal; overflow-wrap: break-word; word-break: break-word;
}
#panel-layer, #panel-window { font-size: 12.5px; color: var(--text-secondary); margin-bottom: 6px; }
.panel-section-label { font-size: 11px; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-muted); margin: 14px 0 6px; }
#panel-upstream, #panel-downstream {
  list-style: none; margin: 0; padding: 0; font-size: 12.5px;
}
#panel-upstream li, #panel-downstream li {
  padding: 5px 8px; margin: 2px 0; border-radius: 6px; cursor: pointer;
  white-space: normal; overflow-wrap: break-word; word-break: break-word;
  transition: background 0.12s ease;
}
#panel-upstream li:hover, #panel-downstream li:hover { background: var(--surface-2); }
#panel-upstream:empty::after, #panel-downstream:empty::after {
  content: "(none — an Entry Node)"; color: var(--text-muted); font-style: italic; display: block; padding: 5px 8px;
}

.node-box {
  cursor: pointer; width: max-content; max-width: 320px; min-width: 160px;
  background: var(--surface-1); border: 1px solid var(--border); border-radius: 10px;
  padding: 10px 14px; box-shadow: 0 1px 2px rgba(11,11,11,0.06);
  border-left: 3px solid var(--series-model);
  transition: opacity 0.15s ease, border-color 0.15s ease, box-shadow 0.15s ease;
}
/* The name itself: full text, always wrapped rather than clipped -- see
   `#panel-title`'s comment above, the same rule applies here. This is
   the element the "no ellipsis on the name" test checks specifically. */
.node-name {
  display: block; font-size: 12.5px; font-weight: 600; color: var(--text-primary);
  white-space: normal; overflow-wrap: break-word; word-break: break-word;
}
.node-layer { display: block; font-size: 10.5px; color: var(--text-muted); margin-top: 2px; text-transform: uppercase; letter-spacing: 0.04em; }
.node-window { display: block; font-size: 11px; color: var(--text-secondary); margin-top: 4px; font-family: ui-monospace, "SF Mono", Menlo, monospace; }

.node-box.selected { border-color: var(--series-model); box-shadow: 0 2px 10px var(--series-model-soft); }
.node-box.highlighted { border-color: var(--series-model); }
.node-box.search-match { border-color: var(--series-model); box-shadow: 0 2px 8px var(--series-model-soft); }
.node-box.dimmed { opacity: 0.28; }

.edge { stroke: var(--gridline); stroke-width: 1.5; fill: none; transition: stroke 0.15s ease, stroke-width 0.15s ease, opacity 0.15s ease; }
.edge.highlighted { stroke: var(--series-model); stroke-width: 2.25; stroke-dasharray: 7 5; animation: flow 0.85s linear infinite; }
.edge.dimmed { opacity: 0.18; }
@keyframes flow { to { stroke-dashoffset: -24; } }
@media (prefers-reduced-motion: reduce) {
  .edge.highlighted { animation: none; }
}
"#;

/// All interactivity is plain client-side JS against the embedded
/// `window.ZHAO_DBT_PLAN_DATA` blob -- no server, no network access, no
/// CDN reference anywhere. Node boxes are real `<div>`s laid out by CSS
/// flex/wrap (not hand-computed pixel coordinates), so a model name never
/// needs measuring or truncating -- the browser's own text layout
/// handles arbitrary length. Edges are drawn afterward, in an absolutely
/// positioned `<svg>` overlay, from each node div's *actual* measured
/// `getBoundingClientRect()` -- so a taller box (from a long wrapped
/// name) never throws edges out of alignment with neighboring nodes.
const JS: &str = r#"
(function () {
  const data = window.ZHAO_DBT_PLAN_DATA;
  const byName = new Map(data.nodes.map((n) => [n.name, n]));

  const upstreamOf = new Map();
  const downstreamOf = new Map();
  for (const e of data.edges) {
    if (!upstreamOf.has(e.downstream)) upstreamOf.set(e.downstream, []);
    upstreamOf.get(e.downstream).push(e);
    if (!downstreamOf.has(e.upstream)) downstreamOf.set(e.upstream, []);
    downstreamOf.get(e.upstream).push(e);
  }

  const layersEl = document.getElementById("layers");
  const edgesSvg = document.getElementById("edges");
  const scrollEl = document.getElementById("graph-scroll");
  const SVG_NS = "http://www.w3.org/2000/svg";

  let selectedName = null;
  const nodeEls = new Map();
  const edgeEls = [];

  function byLayer() {
    const grouped = new Map();
    for (const n of data.nodes) {
      if (!grouped.has(n.layer)) grouped.set(n.layer, []);
      grouped.get(n.layer).push(n);
    }
    const keys = [...grouped.keys()].sort((a, b) => a - b);
    return keys.map((k) => grouped.get(k).slice().sort((a, b) => a.name.localeCompare(b.name)));
  }

  function buildLayout() {
    layersEl.innerHTML = "";
    nodeEls.clear();
    for (const members of byLayer()) {
      const col = document.createElement("div");
      col.className = "layer-col";
      for (const n of members) {
        const box = document.createElement("div");
        box.className = "node-box";
        box.dataset.name = n.name;

        const nameEl = document.createElement("span");
        nameEl.className = "node-name";
        nameEl.textContent = n.name;
        box.appendChild(nameEl);

        const layerEl = document.createElement("span");
        layerEl.className = "node-layer";
        layerEl.textContent = "layer " + n.layer;
        box.appendChild(layerEl);

        const windowEl = document.createElement("span");
        windowEl.className = "node-window";
        windowEl.textContent = n.event_time_start + " .. " + n.event_time_end;
        box.appendChild(windowEl);

        box.addEventListener("click", () => selectNode(n.name));
        col.appendChild(box);
        nodeEls.set(n.name, box);
      }
      layersEl.appendChild(col);
    }
  }

  function el(tag, attrs) {
    const e = document.createElementNS(SVG_NS, tag);
    for (const k in attrs) e.setAttribute(k, attrs[k]);
    return e;
  }

  function edgePath(x1, y1, x2, y2) {
    const dx = Math.max(40, (x2 - x1) * 0.5);
    return `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
  }

  function drawEdges() {
    edgesSvg.innerHTML = "";
    edgeEls.length = 0;
    const w = layersEl.scrollWidth;
    const h = layersEl.scrollHeight;
    edgesSvg.setAttribute("width", w);
    edgesSvg.setAttribute("height", h);
    edgesSvg.setAttribute("viewBox", `0 0 ${w} ${h}`);

    const containerRect = layersEl.getBoundingClientRect();
    for (const e of data.edges) {
      const from = nodeEls.get(e.upstream);
      const to = nodeEls.get(e.downstream);
      if (!from || !to) continue;
      const fr = from.getBoundingClientRect();
      const tr = to.getBoundingClientRect();
      const x1 = fr.right - containerRect.left;
      const y1 = fr.top + fr.height / 2 - containerRect.top;
      const x2 = tr.left - containerRect.left;
      const y2 = tr.top + tr.height / 2 - containerRect.top;
      const path = el("path", { class: "edge", d: edgePath(x1, y1, x2, y2) });
      path.dataset.upstream = e.upstream;
      path.dataset.downstream = e.downstream;
      edgesSvg.appendChild(path);
      edgeEls.push(path);
    }
  }

  function render() {
    buildLayout();
    drawEdges();
    applyHighlight();
  }

  function bfsLevel(startName) {
    const ancestors = new Set();
    const descendants = new Set();
    let frontier = [startName];
    let seen = new Set([startName]);
    while (frontier.length) {
      const next = [];
      for (const name of frontier) {
        for (const e of upstreamOf.get(name) || []) {
          if (!seen.has(e.upstream)) { seen.add(e.upstream); ancestors.add(e.upstream); next.push(e.upstream); }
        }
      }
      frontier = next;
    }
    frontier = [startName];
    seen = new Set([startName]);
    while (frontier.length) {
      const next = [];
      for (const name of frontier) {
        for (const e of downstreamOf.get(name) || []) {
          if (!seen.has(e.downstream)) { seen.add(e.downstream); descendants.add(e.downstream); next.push(e.downstream); }
        }
      }
      frontier = next;
    }
    return { ancestors, descendants };
  }

  function applyHighlight() {
    for (const g of nodeEls.values()) g.classList.remove("selected", "highlighted", "dimmed", "search-match");
    for (const l of edgeEls) l.classList.remove("highlighted", "dimmed");

    const term = document.getElementById("search").value.trim().toLowerCase();
    if (term) {
      for (const [name, g] of nodeEls) {
        if (name.toLowerCase().includes(term)) g.classList.add("search-match");
        else g.classList.add("dimmed");
      }
      for (const l of edgeEls) l.classList.add("dimmed");
      return;
    }

    if (!selectedName) return;
    const { ancestors, descendants } = bfsLevel(selectedName);
    const related = new Set([selectedName, ...ancestors, ...descendants]);
    for (const [name, g] of nodeEls) {
      if (name === selectedName) g.classList.add("selected");
      else if (related.has(name)) g.classList.add("highlighted");
      else g.classList.add("dimmed");
    }
    for (const l of edgeEls) {
      if (related.has(l.dataset.upstream) && related.has(l.dataset.downstream)) l.classList.add("highlighted");
      else l.classList.add("dimmed");
    }
  }

  function renderPanelList(listEl, names) {
    listEl.innerHTML = "";
    for (const name of names) {
      const li = document.createElement("li");
      li.textContent = name;
      li.addEventListener("click", () => selectNode(name));
      listEl.appendChild(li);
    }
  }

  function renderPanel(name) {
    const n = byName.get(name);
    document.getElementById("panel-empty").style.display = "none";
    document.getElementById("panel-content").style.display = "block";
    document.getElementById("panel-title").textContent = n.name;
    document.getElementById("panel-layer").textContent = "Layer " + n.layer;
    document.getElementById("panel-window").textContent = n.event_time_start + " .. " + n.event_time_end;
    renderPanelList(document.getElementById("panel-upstream"), n.depends_on);
    const downstream = (downstreamOf.get(name) || []).map((e) => e.downstream);
    renderPanelList(document.getElementById("panel-downstream"), downstream);
  }

  function selectNode(name) {
    selectedName = name;
    document.getElementById("search").value = "";
    applyHighlight();
    renderPanel(name);
  }

  document.getElementById("search").addEventListener("input", applyHighlight);

  scrollEl.addEventListener("click", (ev) => {
    if (ev.target === scrollEl || ev.target === layersEl || ev.target.id === "edges") {
      selectedName = null;
      applyHighlight();
      document.getElementById("panel-empty").style.display = "block";
      document.getElementById("panel-content").style.display = "none";
    }
  });

  window.addEventListener("resize", drawEdges);

  // Drag `#panel-resize-handle` to resize `#panel`, same convention
  // `lineage_html.rs`'s own resize handle uses. Redraws edges afterward
  // since the graph area's available width changes with the panel.
  (function initPanelResize() {
    const handle = document.getElementById("panel-resize-handle");
    const panel = document.getElementById("panel");
    const mainEl = document.getElementById("main");
    let dragging = false;
    let startX = 0;
    let startWidth = 0;

    handle.addEventListener("mousedown", (ev) => {
      dragging = true;
      startX = ev.clientX;
      startWidth = panel.getBoundingClientRect().width;
      handle.classList.add("resizing");
      document.body.style.userSelect = "none";
      ev.preventDefault();
    });
    document.addEventListener("mousemove", (ev) => {
      if (!dragging) return;
      const delta = startX - ev.clientX;
      const maxWidth = Math.max(240, mainEl.getBoundingClientRect().width - 300);
      const width = Math.min(maxWidth, Math.max(240, startWidth + delta));
      panel.style.width = width + "px";
      drawEdges();
    });
    document.addEventListener("mouseup", () => {
      if (!dragging) return;
      dragging = false;
      handle.classList.remove("resizing");
      document.body.style.userSelect = "";
    });
  })();

  render();
})();
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::date::Date;
    use crate::plan::{AnchorSource, PlannedModel, Window};

    fn sample_plan() -> Plan {
        let d = Date::parse("2026-07-01").unwrap();
        Plan {
            anchor_window: Window { start: d, end: d },
            anchor_source: AnchorSource::Explicit,
            anchor_model: None,
            models: vec![
                PlannedModel {
                    name: "mb_orders_daily".to_string(),
                    window: Window { start: d, end: d },
                    lookback: 0,
                    lookback_unit: crate::date::TimeUnit::Day,
                    lookahead: 0,
                    lookahead_unit: crate::date::TimeUnit::Day,
                    depends_on: Vec::new(),
                    layer: 0,
                },
                PlannedModel {
                    name: "mb_orders_rolling_7d".to_string(),
                    window: Window {
                        start: d.minus_days(3),
                        end: d.plus_days(4),
                    },
                    lookback: 3,
                    lookback_unit: crate::date::TimeUnit::Day,
                    lookahead: 4,
                    lookahead_unit: crate::date::TimeUnit::Day,
                    depends_on: vec!["mb_orders_daily".to_string()],
                    layer: 1,
                },
            ],
            warnings: Vec::new(),
        }
    }

    #[test]
    fn the_default_yesterday_note_appears_as_a_header_banner_only_on_that_path() {
        let mut defaulted = sample_plan();
        defaulted.anchor_source = AnchorSource::DefaultYesterday;
        let html = generate(&defaulted);
        assert!(
            html.contains(r#"<div id="default-yesterday-banner">"#),
            "{html}"
        );
        assert!(html.contains("yesterday"), "{html}");

        // The explicit path (sample_plan()'s default) must render no
        // banner *element* at all -- the CSS rule for it is always
        // present in the stylesheet (it's a plain string constant), so
        // this checks for the `<div>` specifically, not the class name.
        let explicit_html = generate(&sample_plan());
        assert!(
            !explicit_html.contains(r#"<div id="default-yesterday-banner">"#),
            "an explicit-window plan must render no banner element at all: {explicit_html}"
        );
    }

    #[test]
    fn generated_html_contains_no_external_references() {
        let html = generate(&sample_plan());
        let without_svg_namespace = html.replace("http://www.w3.org/2000/svg", "");
        assert!(
            !without_svg_namespace.contains("http://")
                && !without_svg_namespace.contains("https://"),
            "generated HTML must be fully self-contained: {html}"
        );
    }

    #[test]
    fn generated_html_is_a_well_formed_document_with_embedded_data() {
        let html = generate(&sample_plan());
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("<html"));
        assert!(html.contains("<head>"));
        assert!(html.contains("<body>"));
        assert!(html.contains("<style>"));
        assert!(html.contains("<script>"));
        assert!(html.contains("window.ZHAO_DBT_PLAN_DATA"));
    }

    #[test]
    fn generated_html_contains_every_model_name_window_and_layer() {
        let html = generate(&sample_plan());
        assert!(html.contains("mb_orders_daily"));
        assert!(html.contains("mb_orders_rolling_7d"));
        assert!(html.contains("2026-07-01"));
        assert!(html.contains("2026-06-28"));
        assert!(html.contains("2026-07-05"));
        assert!(html.contains("\"layer\":0"));
        assert!(html.contains("\"layer\":1"));
    }

    /// Hard requirement (issue #57): model names must never truncate,
    /// regardless of length -- generate a plan with a deliberately very
    /// long name and assert it comes through the rendered HTML intact.
    #[test]
    fn a_very_long_model_name_is_never_truncated() {
        let d = Date::parse("2026-07-01").unwrap();
        let long_name = "mb_".to_string() + &"x".repeat(300) + "_rolling_window_summary";
        let plan = Plan {
            anchor_window: Window { start: d, end: d },
            anchor_source: AnchorSource::Explicit,
            anchor_model: None,
            models: vec![PlannedModel {
                name: long_name.clone(),
                window: Window { start: d, end: d },
                lookback: 0,
                lookback_unit: crate::date::TimeUnit::Day,
                lookahead: 0,
                lookahead_unit: crate::date::TimeUnit::Day,
                depends_on: Vec::new(),
                layer: 0,
            }],
            warnings: Vec::new(),
        };
        let html = generate(&plan);
        assert!(
            html.contains(&long_name),
            "the full long model name must appear verbatim in the output"
        );
    }

    /// The element that actually renders a model's name client-side
    /// (`.node-name`) must never carry `text-overflow`/`ellipsis` --
    /// `lineage_html.rs`'s own side panel does this for column
    /// expressions (`lineage_html.rs:453`), and this report must not
    /// repeat that pattern for model/node names. A rule on some unrelated
    /// element (e.g. a tooltip) would be fine; this checks the specific
    /// name-rendering class's own CSS block.
    #[test]
    fn the_node_name_css_rule_has_no_ellipsis_or_text_overflow() {
        let block = extract_css_rule(CSS, ".node-name");
        assert!(
            !block.contains("ellipsis") && !block.contains("text-overflow"),
            "the .node-name rule must never truncate: {block}"
        );
        let panel_title_block = extract_css_rule(CSS, "#panel-title");
        assert!(
            !panel_title_block.contains("ellipsis") && !panel_title_block.contains("text-overflow"),
            "the #panel-title rule must never truncate: {panel_title_block}"
        );
    }

    /// Pulls a single `selector { ... }` block's body out of a CSS source
    /// string -- just enough of a parser for this test's purpose (a
    /// handful of known, simple selectors in a hand-written stylesheet),
    /// not a general CSS parser.
    fn extract_css_rule<'a>(css: &'a str, selector: &str) -> &'a str {
        let start = css
            .find(&format!("{selector} {{"))
            .unwrap_or_else(|| panic!("selector {selector:?} not found in CSS"));
        let open = css[start..].find('{').expect("selector has a body") + start;
        let close = css[open..].find('}').expect("selector body is closed") + open;
        &css[open..close]
    }

    #[test]
    fn edges_are_derived_from_every_models_depends_on() {
        let html = generate(&sample_plan());
        assert!(html.contains(r#""upstream":"mb_orders_daily""#));
        assert!(html.contains(r#""downstream":"mb_orders_rolling_7d""#));
    }

    #[test]
    fn default_output_path_lives_in_a_dedicated_directory_and_is_timestamped() {
        let path = default_output_path(Path::new("/tmp/project"));
        assert_eq!(
            path.parent().unwrap(),
            Path::new("/tmp/project/target/zhao/dbt-plan")
        );
        let file_name = path.file_name().unwrap().to_string_lossy();
        assert!(file_name.starts_with("dbt_plan_"));
        assert!(file_name.ends_with(".html"));
        // dbt_plan_ (9) + 14 timestamp digits + .html (5)
        assert_eq!(file_name.len(), 9 + 14 + 5);
    }
}
