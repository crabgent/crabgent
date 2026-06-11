#!/usr/bin/env python3
"""Generate the crabgent functional architecture atlas.

The authored map describes the product architecture. This generator enriches it
with current workspace crates, internal dependencies, PROJECT.md purpose text,
and lightweight Rust surface signals, then writes a single static HTML file.
"""

from __future__ import annotations

import argparse
import html
import json
import re
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_MAP = ROOT / "tools" / "architecture-docs" / "map.json"
DEFAULT_OUTPUT = ROOT / "docs" / "architecture" / "index.html"


PUB_RE = re.compile(
    r"^\s*pub\s+(?:async\s+)?(trait|struct|enum)\s+([A-Za-z_][A-Za-z0-9_]*)"
)
TOOL_NAME_RE = re.compile(r'\b(?:pub\s+)?const\s+TOOL_NAME\s*:\s*&str\s*=\s*"([^"]+)"')
ACTION_RE = re.compile(r"\bAction::([A-Za-z_][A-Za-z0-9_]*)")
CRATE_ROW_RE = re.compile(
    r"^\|\s*\[`(?P<crate>crabgent-[^`]+)`\]\([^)]*\)\s*\|\s*(?P<purpose>.*?)\s*\|$"
)
FEATURE_ROW_RE = re.compile(r"^\|\s*(?P<area>[^|]+?)\s*\|\s*(?P<body>.*?)\s*\|$")


def run(cmd: list[str], cwd: Path = ROOT) -> str:
    proc = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, check=False)
    if proc.returncode != 0:
        raise SystemExit(
            f"{' '.join(cmd)} failed with {proc.returncode}\n{proc.stderr.strip()}"
        )
    return proc.stdout


def load_metadata() -> dict:
    return json.loads(run(["cargo", "metadata", "--no-deps", "--format-version", "1"]))


def parse_project() -> tuple[dict[str, str], dict[str, str]]:
    project = ROOT / "PROJECT.md"
    if not project.exists():
        return {}, {}
    crate_purposes: dict[str, str] = {}
    features: dict[str, str] = {}
    section = ""
    for line in project.read_text(encoding="utf-8").splitlines():
        if line.startswith("## "):
            section = line[3:].strip()
            continue
        if section == "Crate Map":
            match = CRATE_ROW_RE.match(line)
            if match:
                crate_purposes[match.group("crate")] = match.group("purpose").strip()
        elif section == "Feature Surface":
            match = FEATURE_ROW_RE.match(line)
            if match and not match.group("area").strip().startswith("---"):
                area = match.group("area").strip()
                if area != "Area":
                    features[area] = match.group("body").strip()
    return crate_purposes, features


def package_root(package: dict) -> Path:
    return Path(package["manifest_path"]).resolve().parent


def scan_surfaces(root: Path) -> dict[str, list[str]]:
    src = root / "src"
    signals = {"traits": [], "structs": [], "enums": [], "tools": [], "actions": []}
    if not src.exists():
        return signals
    for path in sorted(src.rglob("*.rs")):
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except UnicodeDecodeError:
            continue
        for line in lines:
            if match := PUB_RE.match(line):
                kind, name = match.groups()
                key = {"trait": "traits", "struct": "structs", "enum": "enums"}[kind]
                append_unique(signals[key], name, 14)
            if match := TOOL_NAME_RE.search(line):
                append_unique(signals["tools"], match.group(1), 12)
            for action in ACTION_RE.findall(line):
                append_unique(signals["actions"], action, 16)
    return signals


def append_unique(items: list[str], value: str, limit: int) -> None:
    if value and value not in items and len(items) < limit:
        items.append(value)


def classify_crate(name: str) -> str:
    if name == "crabgent-core":
        return "core"
    if name.startswith("crabgent-channel"):
        return "channels"
    if name.startswith("crabgent-tool"):
        return "tools"
    if name.startswith("crabgent-hook"):
        return "hooks"
    if name in {"crabgent-provider-anthropic", "crabgent-provider-openai", "crabgent-provider-google"}:
        return "providers"
    if name in {
        "crabgent-provider-transport",
        "crabgent-provider-elevenlabs",
        "crabgent-embedding-fastembed",
    }:
        return "providers"
    if (
        name.startswith("crabgent-store")
        or name.startswith("crabgent-memory")
        or name == "crabgent-session"
    ):
        return "state"
    if name in {"crabgent-calendar", "crabgent-cron", "crabgent-task"}:
        return "automation"
    if name.startswith("crabgent-mcp") or name.startswith("crabgent-command"):
        return "integration"
    return "support"


def workspace_packages(metadata: dict, purposes: dict[str, str]) -> dict[str, dict]:
    workspace_ids = set(metadata.get("workspace_members", []))
    packages = {}
    for package in metadata["packages"]:
        if package["id"] not in workspace_ids:
            continue
        name = package["name"]
        internal_deps = sorted(
            {
                dep["name"]
                for dep in package.get("dependencies", [])
                if dep["name"].startswith("crabgent-")
            }
        )
        root = package_root(package)
        packages[name] = {
            "name": name,
            "version": package.get("version", ""),
            "path": str(root.relative_to(ROOT)),
            "manifest": str(Path(package["manifest_path"]).resolve().relative_to(ROOT)),
            "purpose": purposes.get(name, ""),
            "group": classify_crate(name),
            "dependencies": internal_deps,
            "dependedBy": [],
            "surface": scan_surfaces(root),
        }
    for name, crate in packages.items():
        for dep in crate["dependencies"]:
            if dep in packages:
                packages[dep]["dependedBy"].append(name)
    for crate in packages.values():
        crate["dependedBy"] = sorted(crate["dependedBy"])
    return dict(sorted(packages.items()))


def validate_map(atlas: dict, packages: dict[str, dict]) -> list[str]:
    errors: list[str] = []
    map_ids = {m["id"] for m in atlas.get("maps", [])}
    for item in atlas.get("maps", []):
        node_ids = {n["id"] for n in item.get("nodes", [])}
        for node in item.get("nodes", []):
            for crate in node.get("crates", []):
                if crate not in packages:
                    errors.append(
                        f"map {item['id']} node {node['id']} references unknown crate {crate}"
                    )
            target = node.get("targetMap")
            if target and target not in map_ids:
                errors.append(
                    f"map {item['id']} node {node['id']} references unknown map {target}"
                )
        for edge in item.get("edges", []):
            if edge.get("from") not in node_ids:
                errors.append(
                    f"map {item['id']} edge from unknown node {edge.get('from')}"
                )
            if edge.get("to") not in node_ids:
                errors.append(f"map {item['id']} edge to unknown node {edge.get('to')}")
    grouped_crates: dict[str, str] = {}
    for group in atlas.get("capabilityGroups", []):
        group_id = group.get("id", "<missing-id>")
        for crate in group.get("crates", []):
            if crate not in packages:
                errors.append(
                    f"capability group {group_id} references unknown crate {crate}"
                )
            if crate in grouped_crates:
                errors.append(
                    f"crate {crate} appears in capability groups {grouped_crates[crate]} and {group_id}"
                )
            grouped_crates[crate] = group_id
    ungrouped = sorted(set(packages) - set(grouped_crates))
    if ungrouped:
        errors.append("capabilityGroups omit workspace crates: " + ", ".join(ungrouped))
    return errors


def build_atlas(map_file: Path) -> dict:
    authored = json.loads(map_file.read_text(encoding="utf-8"))
    metadata = load_metadata()
    purposes, features = parse_project()
    packages = workspace_packages(metadata, purposes)
    errors = validate_map(authored, packages)
    if errors:
        raise SystemExit("map validation failed:\n" + "\n".join(errors))
    authored["generated"] = {
        "workspacePackageCount": len(packages),
        "source": {
            "cargoMetadata": "cargo metadata --no-deps --format-version 1",
            "project": "PROJECT.md",
            "map": str(map_file.relative_to(ROOT)),
        },
    }
    authored["crates"] = packages
    authored["featureSurface"] = features
    return authored


def json_for_html(value: dict) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).replace(
        "</", "<\\/"
    )


def render_html(atlas: dict) -> str:
    title = html.escape(atlas["title"])
    data = json_for_html(atlas)
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title}</title>
<style>
:root {{
  color-scheme: dark;
  --bg: #10100f;
  --panel: #181714;
  --panel-2: #201e19;
  --line: #3b3830;
  --muted: #a6a097;
  --text: #f3eee4;
  --amber: #ffb454;
  --green: #9bd978;
  --cyan: #67d4c4;
  --red: #e86f5c;
  --violet: #b8a1ff;
  --steel: #9ea7a3;
  --shadow: rgba(0, 0, 0, 0.28);
}}
* {{ box-sizing: border-box; }}
html, body {{ margin: 0; min-height: 100%; background: var(--bg); color: var(--text); font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; letter-spacing: 0; }}
body {{
  background-image:
    linear-gradient(rgba(255,255,255,0.035) 1px, transparent 1px),
    linear-gradient(90deg, rgba(255,255,255,0.03) 1px, transparent 1px);
  background-size: 48px 48px;
}}
button {{ font: inherit; color: inherit; }}
.shell {{ min-height: 100vh; display: grid; grid-template-columns: 260px minmax(520px, 1fr) 360px; grid-template-rows: auto 1fr; gap: 0; }}
.topbar {{ grid-column: 1 / -1; display: flex; align-items: center; justify-content: space-between; gap: 24px; padding: 18px 22px 14px; border-bottom: 1px solid var(--line); background: rgba(16,16,15,0.94); position: sticky; top: 0; z-index: 5; }}
.brand {{ display: flex; flex-direction: column; gap: 3px; min-width: 0; }}
.brand h1 {{ margin: 0; font-size: clamp(20px, 2vw, 32px); line-height: 1; font-weight: 760; text-transform: lowercase; }}
.brand p {{ margin: 0; color: var(--muted); font-size: 13px; max-width: 760px; }}
.stats {{ display: flex; gap: 10px; flex-wrap: wrap; justify-content: flex-end; }}
.stat {{ border: 1px solid var(--line); background: var(--panel); padding: 7px 9px; border-radius: 6px; min-width: 82px; }}
.stat b {{ display: block; font-size: 15px; }}
.stat span {{ display: block; color: var(--muted); font-size: 10px; text-transform: uppercase; }}
.rail {{ border-right: 1px solid var(--line); background: rgba(24,23,20,0.94); padding: 18px 14px; overflow: auto; max-height: calc(100vh - 73px); position: sticky; top: 73px; }}
.rail h2, .inspector h2 {{ margin: 0 0 10px; font-size: 12px; color: var(--muted); text-transform: uppercase; font-weight: 700; }}
.map-button, .crate-chip, .action-button {{ width: 100%; border: 1px solid var(--line); background: transparent; border-radius: 6px; padding: 9px 10px; text-align: left; cursor: pointer; transition: border-color .15s, background .15s, color .15s; }}
.map-button:hover, .crate-chip:hover, .action-button:hover {{ border-color: var(--amber); background: #272219; }}
.map-button.active {{ border-color: var(--amber); background: #312515; color: #fff7e9; }}
.map-list, .group-list {{ display: grid; gap: 8px; margin-bottom: 20px; }}
.map-button small {{ display: block; color: var(--muted); margin-top: 3px; line-height: 1.25; }}
.group {{ margin: 0 0 16px; }}
.group-title {{ color: var(--text); font-size: 13px; margin-bottom: 7px; display: flex; justify-content: space-between; gap: 8px; }}
.group-title span:last-child {{ color: var(--muted); }}
.crate-chip {{ padding: 7px 8px; margin: 0 0 6px; font-size: 12px; line-height: 1.2; color: var(--muted); overflow-wrap: anywhere; }}
.crate-chip.active {{ color: var(--text); border-color: var(--cyan); background: #142522; }}
.main {{ padding: 18px; min-width: 0; }}
.map-head {{ display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 16px; align-items: start; margin-bottom: 14px; }}
.map-head h2 {{ margin: 0 0 4px; font-size: clamp(22px, 3vw, 42px); line-height: 1.02; }}
.map-head p {{ margin: 0; color: var(--muted); max-width: 860px; }}
.crumbs {{ display: flex; gap: 7px; flex-wrap: wrap; justify-content: flex-end; }}
.crumbs button {{ border: 1px solid var(--line); background: var(--panel); border-radius: 6px; padding: 6px 8px; color: var(--muted); cursor: pointer; }}
.graph-frame {{ border: 1px solid var(--line); border-radius: 8px; background: rgba(20,19,16,0.92); box-shadow: 0 14px 40px var(--shadow); overflow: hidden; }}
.graph-toolbar {{ display: flex; justify-content: space-between; gap: 12px; padding: 10px 12px; border-bottom: 1px solid var(--line); color: var(--muted); font-size: 12px; }}
.graph-toolbar code {{ color: var(--cyan); }}
svg {{ display: block; width: 100%; height: min(58vh, 560px); min-height: 420px; }}
.edge {{ stroke-width: 2; fill: none; opacity: .72; }}
.edge.flow {{ stroke: var(--cyan); }}
.edge.policy {{ stroke: var(--amber); stroke-dasharray: 8 7; }}
.edge.tool {{ stroke: var(--violet); }}
.edge.state {{ stroke: var(--green); }}
.edge.runtime {{ stroke: var(--red); stroke-dasharray: 4 6; }}
.edge.dim {{ opacity: .18; }}
.edge-label rect {{ fill: #13120f; stroke: var(--line); }}
.edge-label text {{ fill: var(--muted); font-size: 13px; }}
.node rect {{ stroke-width: 1.5; fill: #1d1b17; stroke: var(--line); rx: 7; }}
.node text {{ pointer-events: none; }}
.node .node-title {{ fill: var(--text); font-size: 18px; font-weight: 740; }}
.node .node-kind {{ fill: var(--muted); font-size: 11px; text-transform: uppercase; }}
.node.boundary rect, .node.adapter rect {{ stroke: var(--cyan); }}
.node.control rect, .node.policy rect {{ stroke: var(--amber); }}
.node.state rect {{ stroke: var(--green); }}
.node.tool rect {{ stroke: var(--violet); }}
.node.llm rect {{ stroke: #f2cf63; }}
.node.runtime rect {{ stroke: var(--red); }}
.node:hover rect {{ stroke-width: 2.5; filter: drop-shadow(0 0 14px rgba(255,180,84,.22)); }}
.node.selected rect {{ fill: #2c2418; stroke-width: 3; }}
.node.highlight rect {{ fill: #17302b; stroke-width: 2.4; }}
.node.dim {{ opacity: .3; }}
.legend-strip {{ display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 8px; margin-top: 12px; }}
.legend-item {{ border: 1px solid var(--line); border-radius: 6px; padding: 8px; color: var(--muted); font-size: 12px; background: rgba(24,23,20,.75); }}
.legend-item span {{ display: inline-block; width: 22px; height: 3px; margin-right: 6px; vertical-align: middle; }}
.inspector {{ border-left: 1px solid var(--line); background: rgba(24,23,20,0.96); padding: 18px 16px 28px; overflow: auto; max-height: calc(100vh - 73px); position: sticky; top: 73px; }}
.panel-title {{ margin: 0 0 6px; font-size: 22px; line-height: 1.08; }}
.summary {{ color: var(--muted); line-height: 1.45; margin: 0 0 14px; }}
.section {{ border-top: 1px solid var(--line); padding-top: 13px; margin-top: 13px; }}
.section h3 {{ margin: 0 0 9px; font-size: 12px; color: var(--muted); text-transform: uppercase; }}
.pill-list {{ display: flex; flex-wrap: wrap; gap: 7px; }}
.pill {{ border: 1px solid var(--line); background: #141310; color: var(--muted); border-radius: 999px; padding: 5px 8px; font-size: 12px; max-width: 100%; overflow-wrap: anywhere; }}
.pill.clickable {{ cursor: pointer; }}
.pill.clickable:hover {{ color: var(--text); border-color: var(--cyan); }}
.facts {{ display: grid; gap: 8px; }}
.fact {{ display: grid; grid-template-columns: 94px minmax(0, 1fr); gap: 9px; font-size: 13px; }}
.fact span:first-child {{ color: var(--muted); }}
.fact span:last-child {{ overflow-wrap: anywhere; }}
.edge-list {{ display: grid; gap: 8px; }}
.edge-card {{ border: 1px solid var(--line); border-radius: 6px; padding: 8px; font-size: 12px; color: var(--muted); background: #141310; }}
.edge-card b {{ color: var(--text); }}
.action-button {{ color: var(--text); text-align: center; margin-top: 8px; }}
.action-row {{ display: grid; grid-template-columns: 1fr 1fr; gap: 8px; }}
.empty {{ color: var(--muted); font-size: 13px; }}
@media (max-width: 1120px) {{
  .shell {{ grid-template-columns: 220px minmax(0, 1fr); }}
  .inspector {{ grid-column: 1 / -1; border-left: 0; border-top: 1px solid var(--line); max-height: none; position: static; }}
  .rail {{ max-height: none; position: static; }}
}}
@media (max-width: 780px) {{
  .shell {{ display: block; }}
  .topbar {{ position: static; align-items: flex-start; flex-direction: column; }}
  .rail {{ border-right: 0; border-bottom: 1px solid var(--line); }}
  .main {{ padding: 12px; }}
  .map-head {{ grid-template-columns: 1fr; }}
  .legend-strip {{ grid-template-columns: 1fr 1fr; }}
  svg {{ min-height: 380px; height: 58vh; }}
}}
</style>
</head>
<body>
<div class="shell">
  <header class="topbar">
    <div class="brand">
      <h1>crabgent architecture atlas</h1>
      <p id="tagline"></p>
    </div>
    <div class="stats" id="stats"></div>
  </header>
  <aside class="rail">
    <h2>Function Maps</h2>
    <div class="map-list" id="mapList"></div>
    <h2>Crate Underside</h2>
    <div id="crateGroups"></div>
  </aside>
  <main class="main">
    <div class="map-head">
      <div>
        <h2 id="mapTitle"></h2>
        <p id="mapSubtitle"></p>
      </div>
      <div class="crumbs" id="crumbs"></div>
    </div>
    <div class="graph-frame">
      <div class="graph-toolbar">
        <span id="mapLegend"></span>
        <span><code>click</code> select · <code>double</code> drill</span>
      </div>
      <svg id="graph" viewBox="0 0 1160 650" role="img" aria-labelledby="mapTitle mapSubtitle"></svg>
    </div>
    <div class="legend-strip">
      <div class="legend-item"><span style="background:var(--cyan)"></span>runtime flow</div>
      <div class="legend-item"><span style="background:var(--amber)"></span>policy/trust</div>
      <div class="legend-item"><span style="background:var(--violet)"></span>tool loop</div>
      <div class="legend-item"><span style="background:var(--green)"></span>state/recall</div>
      <div class="legend-item"><span style="background:var(--red)"></span>lifecycle</div>
    </div>
  </main>
  <aside class="inspector" id="inspector"></aside>
</div>
<script id="atlas-data" type="application/json">{data}</script>
<script>
const atlas = JSON.parse(document.getElementById("atlas-data").textContent);
const maps = new Map(atlas.maps.map(item => [item.id, item]));
const crates = atlas.crates;
const state = {{ mapId: atlas.entryMap, selectedNodeId: null, selectedCrate: null, history: [] }};
const svgNS = "http://www.w3.org/2000/svg";

function el(tag, attrs = {{}}, text = "") {{
  const node = document.createElement(tag);
  for (const [key, value] of Object.entries(attrs)) {{
    if (key === "class") node.className = value;
    else if (key.startsWith("on")) node.addEventListener(key.slice(2), value);
    else node.setAttribute(key, value);
  }}
  if (text) node.textContent = text;
  return node;
}}

function svgel(tag, attrs = {{}}, text = "") {{
  const node = document.createElementNS(svgNS, tag);
  for (const [key, value] of Object.entries(attrs)) node.setAttribute(key, value);
  if (text) node.textContent = text;
  return node;
}}

function currentMap() {{ return maps.get(state.mapId); }}

function setMap(id, pushHistory = true) {{
  if (!maps.has(id)) return;
  if (pushHistory && state.mapId !== id) state.history.push(state.mapId);
  state.mapId = id;
  state.selectedNodeId = null;
  state.selectedCrate = null;
  render();
}}

function selectNode(id) {{
  if (state.selectedNodeId === id) {{
    const node = currentMap().nodes.find(item => item.id === id);
    if (node && node.targetMap && node.targetMap !== state.mapId) setMap(node.targetMap);
    return;
  }}
  state.selectedNodeId = id;
  state.selectedCrate = null;
  render();
}}

function selectCrate(name) {{
  state.selectedCrate = state.selectedCrate === name ? null : name;
  state.selectedNodeId = null;
  render();
}}

function render() {{
  document.getElementById("tagline").textContent = atlas.tagline;
  renderStats();
  renderMapList();
  renderCrateGroups();
  renderMapHead();
  renderGraph();
  renderInspector();
}}

function renderStats() {{
  const stats = document.getElementById("stats");
  stats.replaceChildren();
  stats.append(
    stat(String(atlas.generated.workspacePackageCount), "workspace crates"),
    stat(String(atlas.maps.length), "function maps"),
    stat(String(Object.keys(atlas.featureSurface).length), "feature rows")
  );
}}

function stat(value, label) {{
  const box = el("div", {{class: "stat"}});
  box.append(el("b", {{}}, value), el("span", {{}}, label));
  return box;
}}

function renderMapList() {{
  const list = document.getElementById("mapList");
  list.replaceChildren();
  for (const item of atlas.maps) {{
    const button = el("button", {{class: "map-button" + (item.id === state.mapId ? " active" : ""), onclick: () => setMap(item.id)}});
    button.append(document.createTextNode(item.title), el("small", {{}}, item.subtitle));
    list.append(button);
  }}
}}

function renderCrateGroups() {{
  const root = document.getElementById("crateGroups");
  root.replaceChildren();
  for (const group of atlas.capabilityGroups) {{
    const wrap = el("div", {{class: "group"}});
    const known = group.crates.filter(name => crates[name]);
    const title = el("div", {{class: "group-title"}});
    title.append(el("span", {{}}, group.label), el("span", {{}}, String(known.length)));
    wrap.append(title);
    for (const crate of known) {{
      wrap.append(el("button", {{
        class: "crate-chip" + (crate === state.selectedCrate ? " active" : ""),
        onclick: () => selectCrate(crate)
      }}, crate));
    }}
    root.append(wrap);
  }}
}}

function renderMapHead() {{
  const map = currentMap();
  document.getElementById("mapTitle").textContent = map.title;
  document.getElementById("mapSubtitle").textContent = map.subtitle;
  document.getElementById("mapLegend").textContent = map.legend || "";
  const crumbs = document.getElementById("crumbs");
  crumbs.replaceChildren();
  if (state.history.length) {{
    crumbs.append(el("button", {{onclick: () => setMap(state.history.pop(), false)}}, "Back"));
  }}
  crumbs.append(el("button", {{onclick: () => setMap(atlas.entryMap)}}, "Runtime spine"));
}}

function renderGraph() {{
  const map = currentMap();
  const graph = document.getElementById("graph");
  graph.replaceChildren();
  const box = viewBoxFor(map);
  graph.setAttribute("viewBox", `${{box.x}} ${{box.y}} ${{box.w}} ${{box.h}}`);
  const defs = svgel("defs");
  defs.append(marker("flow", "var(--cyan)"), marker("policy", "var(--amber)"), marker("tool", "var(--violet)"), marker("state", "var(--green)"), marker("runtime", "var(--red)"));
  graph.append(defs);
  graph.append(gridLayer(box));
  const byId = new Map(map.nodes.map(node => [node.id, node]));
  const highlightedNodeIds = new Set();
  if (state.selectedCrate) {{
    for (const node of map.nodes) {{
      if ((node.crates || []).includes(state.selectedCrate)) highlightedNodeIds.add(node.id);
    }}
  }}
  for (const edge of map.edges || []) {{
    const from = byId.get(edge.from);
    const to = byId.get(edge.to);
    if (!from || !to) continue;
    graph.append(renderEdge(edge, from, to, highlightedNodeIds));
  }}
  for (const node of map.nodes) {{
    graph.append(renderNode(node, highlightedNodeIds));
  }}
}}

function viewBoxFor(map) {{
  const xs = map.nodes.flatMap(node => [node.x, node.x + node.w]);
  const ys = map.nodes.flatMap(node => [node.y, node.y + node.h]);
  const pad = 70;
  const minX = Math.min(...xs) - pad;
  const minY = Math.min(...ys) - pad;
  const maxX = Math.max(...xs) + pad;
  const maxY = Math.max(...ys) + pad;
  return {{x: minX, y: minY, w: Math.max(760, maxX - minX), h: Math.max(460, maxY - minY)}};
}}

function marker(kind, color) {{
  const marker = svgel("marker", {{id: "arrow-" + kind, viewBox: "0 0 10 10", refX: "8", refY: "5", markerWidth: "7", markerHeight: "7", orient: "auto-start-reverse"}});
  marker.append(svgel("path", {{d: "M 0 0 L 10 5 L 0 10 z", fill: color}}));
  return marker;
}}

function gridLayer(box) {{
  const group = svgel("g", {{"aria-hidden": "true", opacity: "0.45"}});
  const startX = Math.floor(box.x / 80) * 80;
  const startY = Math.floor(box.y / 80) * 80;
  for (let x = startX; x < box.x + box.w; x += 80) group.append(svgel("line", {{x1: x, y1: box.y, x2: x, y2: box.y + box.h, stroke: "#24221d", "stroke-width": "1"}}));
  for (let y = startY; y < box.y + box.h; y += 80) group.append(svgel("line", {{x1: box.x, y1: y, x2: box.x + box.w, y2: y, stroke: "#24221d", "stroke-width": "1"}}));
  return group;
}}

function renderEdge(edge, from, to, highlightedNodeIds) {{
  const group = svgel("g", {{class: "edge-label"}});
  const start = pointToward(from, to);
  const end = pointToward(to, from);
  const dx = end.x - start.x;
  const dy = end.y - start.y;
  const bend = Math.min(70, Math.max(-70, dx * 0.08));
  const c1 = {{x: start.x + dx * 0.38, y: start.y + dy * 0.22 - bend}};
  const c2 = {{x: start.x + dx * 0.62, y: start.y + dy * 0.78 + bend}};
  const dim = state.selectedCrate && !(highlightedNodeIds.has(edge.from) || highlightedNodeIds.has(edge.to));
  const kind = edge.kind || "flow";
  const path = svgel("path", {{
    d: `M ${{start.x}} ${{start.y}} C ${{c1.x}} ${{c1.y}}, ${{c2.x}} ${{c2.y}}, ${{end.x}} ${{end.y}}`,
    class: `edge ${{kind}}${{dim ? " dim" : ""}}`,
    "marker-end": `url(#arrow-${{kind}})`
  }});
  group.append(path);
  if (edge.label) {{
    const mid = cubic(start, c1, c2, end, 0.5);
    const text = svgel("text", {{x: mid.x, y: mid.y, "text-anchor": "middle"}}, edge.label);
    const approx = Math.max(46, edge.label.length * 6 + 14);
    group.append(svgel("rect", {{x: mid.x - approx / 2, y: mid.y - 15, width: approx, height: 20, rx: 5}}));
    group.append(text);
  }}
  return group;
}}

function pointToward(a, b) {{
  const ac = center(a);
  const bc = center(b);
  const dx = bc.x - ac.x;
  const dy = bc.y - ac.y;
  if (Math.abs(dx / Math.max(1, a.w)) > Math.abs(dy / Math.max(1, a.h))) {{
    return {{x: ac.x + Math.sign(dx) * a.w / 2, y: ac.y + dy * 0.12}};
  }}
  return {{x: ac.x + dx * 0.12, y: ac.y + Math.sign(dy) * a.h / 2}};
}}

function center(node) {{ return {{x: node.x + node.w / 2, y: node.y + node.h / 2}}; }}

function cubic(p0, p1, p2, p3, t) {{
  const u = 1 - t;
  return {{
    x: u*u*u*p0.x + 3*u*u*t*p1.x + 3*u*t*t*p2.x + t*t*t*p3.x,
    y: u*u*u*p0.y + 3*u*u*t*p1.y + 3*u*t*t*p2.y + t*t*t*p3.y
  }};
}}

function renderNode(node, highlightedNodeIds) {{
  const selected = state.selectedNodeId === node.id;
  const highlighted = highlightedNodeIds.has(node.id);
  const dim = state.selectedCrate && !highlighted;
  const group = svgel("g", {{
    class: `node ${{node.kind || "runtime"}}${{selected ? " selected" : ""}}${{highlighted ? " highlight" : ""}}${{dim ? " dim" : ""}}`,
    transform: `translate(${{node.x}} ${{node.y}})`,
    role: "button",
    tabindex: "0",
    "aria-label": node.label
  }});
  group.addEventListener("click", () => selectNode(node.id));
  group.addEventListener("dblclick", () => node.targetMap && setMap(node.targetMap));
  group.addEventListener("keydown", event => {{
    if (event.key === "Enter" || event.key === " ") selectNode(node.id);
  }});
  group.append(svgel("rect", {{width: node.w, height: node.h}}));
  const title = svgel("text", {{class: "node-title", x: 13, y: 24}});
  wrapSvgText(title, node.label, Math.max(12, Math.floor(node.w / 9)), 16);
  group.append(title);
  group.append(svgel("text", {{class: "node-kind", x: 13, y: node.h - 13}}, node.kind || "runtime"));
  if (node.targetMap && node.targetMap !== state.mapId) {{
    group.append(svgel("circle", {{cx: node.w - 16, cy: 16, r: 5, fill: "var(--amber)"}}));
  }}
  return group;
}}

function wrapSvgText(textNode, value, maxChars, lineHeight) {{
  const words = String(value).split(/\\s+/);
  let line = "";
  let dy = 0;
  for (const word of words) {{
    const next = line ? `${{line}} ${{word}}` : word;
    if (next.length > maxChars && line) {{
      textNode.append(svgel("tspan", {{x: textNode.getAttribute("x"), dy: dy ? lineHeight : 0}}, line));
      line = word;
      dy += lineHeight;
    }} else {{
      line = next;
    }}
  }}
  if (line) textNode.append(svgel("tspan", {{x: textNode.getAttribute("x"), dy: dy ? lineHeight : 0}}, line));
}}

function renderInspector() {{
  const root = document.getElementById("inspector");
  root.replaceChildren();
  if (state.selectedCrate) return renderCrateInspector(root, state.selectedCrate);
  if (state.selectedNodeId) {{
    const node = currentMap().nodes.find(item => item.id === state.selectedNodeId);
    if (node) return renderNodeInspector(root, node);
  }}
  renderMapInspector(root);
}}

function renderMapInspector(root) {{
  const map = currentMap();
  root.append(el("h2", {{}}, "Current Map"));
  root.append(el("h1", {{class: "panel-title"}}, map.title));
  root.append(el("p", {{class: "summary"}}, map.legend || map.subtitle));
  const section = sectionBlock("Signal Lines");
  const list = el("div", {{class: "edge-list"}});
  for (const edge of map.edges || []) {{
    const from = map.nodes.find(node => node.id === edge.from)?.label || edge.from;
    const to = map.nodes.find(node => node.id === edge.to)?.label || edge.to;
    const card = el("div", {{class: "edge-card"}});
    card.innerHTML = `<b>${{escapeHtml(from)}}</b> -> <b>${{escapeHtml(to)}}</b><br>${{escapeHtml(edge.label || edge.kind || "")}}`;
    list.append(card);
  }}
  section.append(list);
  root.append(section);
  const principles = sectionBlock("Spirit Pressure");
  const pills = el("div", {{class: "pill-list"}});
  for (const principle of atlas.principles) pills.append(el("span", {{class: "pill"}}, principle));
  principles.append(pills);
  root.append(principles);
}}

function renderNodeInspector(root, node) {{
  root.append(el("h2", {{}}, "Function Node"));
  root.append(el("h1", {{class: "panel-title"}}, node.label));
  root.append(el("p", {{class: "summary"}}, node.summary || ""));
  if (node.targetMap && node.targetMap !== state.mapId) {{
    root.append(el("button", {{class: "action-button", onclick: () => setMap(node.targetMap)}}, "Open detail map"));
  }}
  root.append(facts({{kind: node.kind || "runtime", map: currentMap().title}}));
  root.append(listSection("Surface Signals", node.surface || []));
  root.append(crateSection(node.crates || []));
}}

function renderCrateInspector(root, name) {{
  const crate = crates[name];
  if (!crate) return;
  root.append(el("h2", {{}}, "Crate Underside"));
  root.append(el("h1", {{class: "panel-title"}}, name));
  root.append(el("p", {{class: "summary"}}, crate.purpose || "No PROJECT.md purpose text found."));
  root.append(facts({{group: crate.group, manifest: crate.manifest, version: crate.version}}));
  const used = atlas.maps.flatMap(map => map.nodes.filter(node => (node.crates || []).includes(name)).map(node => `${{map.title}} / ${{node.label}}`));
  root.append(listSection("Used By Function Nodes", used));
  root.append(listSection("Depends On", crate.dependencies));
  root.append(listSection("Depended On By", crate.dependedBy));
  const surface = [];
  for (const [label, items] of Object.entries(crate.surface || {{}})) {{
    if (items.length) surface.push(`${{label}}: ${{items.join(", ")}}`);
  }}
  root.append(listSection("Rust Surface Signals", surface));
}}

function facts(obj) {{
  const section = sectionBlock("Facts");
  const wrap = el("div", {{class: "facts"}});
  for (const [key, value] of Object.entries(obj)) {{
    const row = el("div", {{class: "fact"}});
    row.append(el("span", {{}}, key), el("span", {{}}, String(value || "")));
    wrap.append(row);
  }}
  section.append(wrap);
  return section;
}}

function sectionBlock(title) {{
  const section = el("section", {{class: "section"}});
  section.append(el("h3", {{}}, title));
  return section;
}}

function listSection(title, items) {{
  const section = sectionBlock(title);
  if (!items || !items.length) {{
    section.append(el("div", {{class: "empty"}}, "None detected."));
    return section;
  }}
  const list = el("div", {{class: "pill-list"}});
  for (const item of items) list.append(el("span", {{class: "pill"}}, item));
  section.append(list);
  return section;
}}

function crateSection(names) {{
  const section = sectionBlock("Implementing Crates");
  if (!names.length) {{
    section.append(el("div", {{class: "empty"}}, "No crate mapping."));
    return section;
  }}
  const list = el("div", {{class: "pill-list"}});
  for (const name of names) {{
    list.append(el("button", {{class: "pill clickable", onclick: () => selectCrate(name)}}, name));
  }}
  section.append(list);
  return section;
}}

function escapeHtml(value) {{
  return String(value).replace(/[&<>"']/g, char => ({{"&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#039;"}}[char]));
}}

render();
</script>
</body>
</html>
"""


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Generate docs/architecture/index.html"
    )
    parser.add_argument("--map", default=str(DEFAULT_MAP), help="authored map JSON")
    parser.add_argument(
        "--output", default=str(DEFAULT_OUTPUT), help="output HTML path"
    )
    parser.add_argument("--check", action="store_true", help="fail if output is stale")
    args = parser.parse_args()

    map_file = Path(args.map).resolve()
    output = Path(args.output).resolve()
    atlas = build_atlas(map_file)
    rendered = render_html(atlas)

    if args.check:
        current = output.read_text(encoding="utf-8") if output.exists() else ""
        if current != rendered:
            print(f"{output.relative_to(ROOT)} is stale", file=sys.stderr)
            return 1
        print(f"{output.relative_to(ROOT)} is current")
        return 0

    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(rendered, encoding="utf-8")
    print(f"wrote {output.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
