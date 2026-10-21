"""Render measurements for people; machine data stays in report.json."""
import argparse
import html
import json
from pathlib import Path
import shlex

ENGINES = ("upstream", "river-raptor", "ghostty")
DISPLAY_ENGINES = ("river-raptor", "upstream", "ghostty")


def esc(value):
    return html.escape(str(value), quote=True)


def timing(value):
    if value is None:
        return "—"
    if value >= 1_000_000:
        return f"{value / 1_000_000:,.2f} ms"
    if value >= 1000:
        return f"{value / 1000:,.2f} µs"
    return f"{value:,.2f} ns"


def fields(items):
    return '<dl>' + ''.join(f'<dt>{esc(k)}</dt><dd>{esc(v)}</dd>'
                            for k, v in items if v is not None) + '</dl>'


def sample_table(result, phase, root):
    groups = result.get(phase, {})
    count = max((len(v) for v in groups.values()), default=0)
    if not count:
        return "<p>No samples recorded.</p>"
    rows = []
    for index in range(count):
        cells = []
        for engine in DISPLAY_ENGINES:
            samples = groups.get(engine, [])
            if index >= len(samples):
                cells.append("<td>—</td>")
                continue
            sample = samples[index]
            raw = sample.get("rio_report") or {}
            setup = sample.get("setup") or raw.get("setup_metrics")
            setup_text = (f'<br><small>Setup: {setup["columns"]} × {setup["rows"]}, '
                          f'{setup["history_lines"]:,} history lines</small>' if setup else "")
            links = []
            for stream in ("stdout", "stderr"):
                if sample.get(stream) and root:
                    try:
                        relative = Path(sample[stream]).relative_to(root).as_posix()
                        links.append(f'<a href="{esc(relative)}">{stream}</a>')
                    except ValueError:
                        pass
            cells.append(f'<td><b>{sample["ns_per_operation"]:,.1f}</b> ns/op'
                         f'<br><small>Step {timing(sample.get("step_ns"))} · '
                         f'process {timing(sample.get("wall_ns"))}</small>{setup_text}<br>{" · ".join(links)}</td>')
        rows.append(f'<tr><th>{index + 1}</th>{"".join(cells)}</tr>')
    return ('<div class="scroll"><table class="samples"><thead><tr><th>Sample</th>'
            '<th>River-raptor</th><th>Upstream</th><th>Ghostty</th></tr></thead><tbody>'
            + ''.join(rows) + '</tbody></table></div>')


def case_details(result, root):
    commands, metadata = [], []
    for engine in DISPLAY_ENGINES:
        samples = result.get("samples", {}).get(engine, [])
        if not samples:
            continue
        first = samples[0]
        commands.append(f'<p><b>{esc(engine)}</b></p><pre>{esc(shlex.join(first.get("command", [])))}</pre>')
        raw = first.get("rio_report")
        if raw:
            metadata.append(f'<h4>{esc(engine)}</h4>' + fields([
                ("Input bytes", f'{raw.get("input_bytes", 0):,}'), ("Checksum", raw.get("checksum")),
                ("Snapshot API", raw.get("snapshot_api")), ("Unicode API", raw.get("unicode_api")),
                ("Formatter API", raw.get("formatter_api")), ("Extras API", raw.get("extras_api")),
                *[(key.replace("_", " "), value) for key, value in raw.get("options", {}).items()],
            ]))
    order = ''.join(f'<li>{esc(item["phase"])} {item["index"] + 1}: '
                    f'{esc(" → ".join(item["engines"]))}</li>' for item in result.get("order", []))
    return (f'<details class="case-details"><summary>Samples and setup</summary>'
            f'<p>{esc(result["notes"])}</p><p>{result["operations"]:,} operations per internal step. '
            'Process time includes startup and teardown.</p><h4>Measured samples</h4>'
            + sample_table(result, "samples", root)
            + '<details><summary>Warmup samples</summary>' + sample_table(result, "warmups", root) + '</details>'
            + '<details><summary>Commands and execution order</summary>' + ''.join(commands)
            + '<ol>' + order + '</ol></details><details><summary>Checksums and engine settings</summary>'
            '<p>Ghostty exposes no semantic checksum. Checksums are checked for stability within each Rio '
            'engine; cross-engine equivalence is not established.</p>' + ''.join(metadata) + '</details></details>')


def provenance(report):
    engines = []
    for name, data in report.get("engines", {}).items():
        patches = ''.join(f'<li><b>{esc(p["name"])}</b> · {esc(p["status"])}'
                          f'<br><code>SHA-256 {esc(p["sha256"])}</code></li>' for p in data.get("patches", []))
        build = fields([
            ("Source ref", data.get("ref")), ("Commit", data.get("commit")),
            ("Git tree", data.get("tree")), ("Compiler", data.get("toolchain")),
            ("Binary SHA-256", data.get("binary", {}).get("sha256")),
            ("Effective patch SHA-256", data.get("effective_diff_sha256")),
            ("Harness commit", data.get("harness", {}).get("commit")),
            ("Harness archive SHA-256", data.get("harness", {}).get("archive_sha256")),
            ("Worktree changes", data.get("worktree_status") or "Clean"),
        ])
        engines.append(f'<details><summary>{esc(name)} source and build</summary>{build}'
                       f'<p>{esc(data.get("patch_purpose", ""))}</p>'
                       f'<ul>{patches or "<li>No adapter patches</li>"}</ul>'
                       f'<pre>{esc(shlex.join(data.get("build_command", [])))}</pre>'
                       + fields(data.get("build_environment", {}).items()) + '</details>')
    host = report.get("host", {})
    settings = fields([
        ("Started UTC", report.get("started_utc")), ("Finished UTC", report.get("finished_utc")),
        ("Platform", host.get("platform")), ("CPU", host.get("cpu_model", host.get("machine"))),
        ("Pinned logical CPU", report.get("cpu")), ("Governor", host.get("cpu_governor")),
        ("Python", host.get("python")), ("Driver commit", report.get("driver", {}).get("commit")),
        ("Driver checkout changes", report.get("driver", {}).get("status") or "Clean"),
        *[(k.replace("_", " ").capitalize(), v) for k, v in report.get("options", {}).items()],
    ])
    corpora = report.get("corpora", {})
    corpus_rows = []
    for group, files in (("Short", corpora.get("short", {}).get("files", {})),
                         ("Stream", corpora.get("stream", {}))):
        for name, data in files.items():
            corpus_rows.append(f'<tr><td>{group}</td><td>{esc(name)}</td><td>{data["bytes"]:,}</td>'
                               f'<td><code>{esc(data["sha256"])}</code></td></tr>')
    return ('<section><h2>Reproduction details</h2><details><summary>Host and run settings</summary>'
            + settings + '</details>' + ''.join(engines)
            + '<details><summary>Input corpora and checksums</summary><p>Sizes can exceed the requested '
            'target because corpora repeat complete generation units. The fixture design reference is '
            + esc(corpora.get("short", {}).get("ghostty_revision", "unknown"))
            + '; actual tested commits are shown above.</p><div class="scroll"><table>'
            '<thead><tr><th>Corpus</th><th>File</th><th>Bytes</th><th>SHA-256</th></tr></thead><tbody>'
            + ''.join(corpus_rows) + '</tbody></table></div></details></section>')


def render(report, directory=None):
    roots = [Path(e["binary"]["path"]).parent for e in report.get("engines", {}).values() if e.get("binary")]
    root = Path(directory) if directory is not None else (roots[0] if roots else None)
    rows = []
    for result in report["results"]:
        medians = result.get("median_ns", {})
        reference = medians.get("river-raptor")
        cells = []
        for engine in DISPLAY_ENGINES:
            values = [s["ns_per_operation"] for s in result["samples"].get(engine, [])]
            value = medians.get(engine)
            spread = f'{timing(min(values))}–{timing(max(values))} · n={len(values)}' if values else "No measurements"
            relative = value / reference if value is not None and reference else None
            # A linear time axis: the reference occupies half the track. Keep
            # exact deltas visible when a >2x cost exceeds the track's range.
            width = min(100, max(0, relative * 50)) if relative is not None else 0
            delta = (relative - 1) * 100 if relative is not None else None
            label = ("Reference · 0.0%" if engine == "river-raptor" and relative is not None
                     else f'{"+" if delta >= 0 else "−"}{abs(delta):,.1f}% time' if delta is not None else "No reference")
            overflow = '<span class="overflow" title="Time exceeds twice the reference">→</span>' if relative is not None and relative > 2 else ""
            cells.append(f'<td class="measurement"><b>{timing(value)}</b><div class="track">'
                         f'<span class="bar {engine}" style="width:{width:.2f}%"></span>{overflow}</div>'
                         f'<small class="delta">{label}</small><br>'
                         f'<small>{spread}</small></td>')
        ratios, ratio_values = [], {}
        for baseline in ("upstream", "ghostty"):
            ratio = medians.get(baseline, 0) / medians["river-raptor"] if medians.get("river-raptor", 0) else None
            ratio_values[baseline] = ratio or 0
            cls = "good" if ratio is not None and ratio >= 1 else "slow"
            label = "faster" if ratio is not None and ratio >= 1 else "slower"
            factor = ratio if ratio is not None and ratio >= 1 else (1 / ratio if ratio else 0)
            if baseline == "ghostty" and result["action"] == "screen-clone" and result["mode"] == "clone" and ratio:
                ratios.append(f'<td><b>Different work</b><br><small>{ratio:.2f}× native cost ratio</small></td>')
            else:
                ratios.append(f'<td class="{cls}"><b>{factor:.2f}×</b><br><small>{label}</small></td>'
                              if ratio else '<td>—</td>')
        failure = f'<p class="slow">Failed: {esc(result["error"])}</p>' if result.get("error") else ""
        large_ratio = any(r >= 10 or 0 < r <= 0.1 for r in ratio_values.values())
        qualification = f'<p class="qualification">{esc(result["notes"])}</p>' if large_ratio and not result["control"] else ""
        name = esc(result["case"])
        rows.append(f'<tr class="case-row" data-name="{name}" data-control="{str(result["control"]).lower()}" '
                    f'data-ghostty="{ratio_values["ghostty"]}" data-upstream="{ratio_values["upstream"]}">'
                    f'<td><b>{name}</b>{" <span class=tag>control</span>" if result["control"] else ""}'
                    f'{failure}{qualification}{case_details(result, root)}</td>{"".join(cells + ratios)}</tr>')
    cards = []
    for engine in DISPLAY_ENGINES:
        value = report.get("engines", {}).get(engine, {})
        patches = value.get("patches", [])
        cards.append(f'<article class="engine {engine}"><h3>{esc(engine)}</h3>'
                     f'<code>{esc(value.get("commit", "unresolved"))}</code>'
                     f'<p>{esc(value.get("toolchain", "Build pending"))}</p><small>{len(patches)} adapter patches · '
                     f'{esc(", ".join(p["name"] for p in patches) or "Original branch core")}</small></article>')
    error = f'<p class="error">{esc(report["error"])}</p>' if report.get("error") else ""
    options = report.get("options", {})
    completed = sum(bool(r.get("median_ns")) for r in report["results"])
    return f'''<!doctype html><html lang="en"><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Terminal benchmark · {esc(report["id"])}</title><style>
:root{{color-scheme:dark;--muted:#9eb1bf;--up:#71b8ff;--river:#6ce1b0;--ghost:#f3be77}}
*{{box-sizing:border-box}}body{{font:14px system-ui;background:#101820;color:#e4edf3;margin:0;line-height:1.55}}
main{{max-width:1600px;margin:auto;padding:32px}}h1{{font-size:32px;margin:4px 0}}h2{{font-size:22px;margin-top:34px}}
h3{{font-size:17px;margin:0 0 8px}}h4{{margin:18px 0 6px}}p{{margin:10px 0}}a{{color:#89ceff}}
.muted,small,dt{{color:var(--muted)}}small{{font-size:11px}}code,pre{{font:12px ui-monospace,monospace;overflow-wrap:anywhere}}
.engines{{display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:14px;margin:24px 0}}
.engine{{padding:18px;background:#192732;border:1px solid #344753;border-top:3px solid;border-radius:8px}}
.engine.upstream{{border-top-color:var(--up)}}.engine.river-raptor{{border-top-color:var(--river)}}.engine.ghostty{{border-top-color:var(--ghost)}}
.engine code{{display:block}}.stats{{display:flex;gap:24px;flex-wrap:wrap;margin:18px 0}}.stats b{{font-size:21px}}
.toolbar{{display:flex;gap:14px;align-items:center;flex-wrap:wrap;margin:20px 0 12px}}
input,select{{font:inherit;padding:9px;background:#21333f;border:1px solid #536b7b;border-radius:5px;color:inherit}}
input[type=checkbox]{{accent-color:var(--river)}}.scroll{{overflow:auto}}table{{border-collapse:collapse;width:100%}}
th,td{{padding:14px 12px;text-align:left;border-bottom:1px solid #30434f;vertical-align:top}}
thead th{{background:#1c2c37;font-size:12px;white-space:nowrap}}#cases>tr:hover{{background:#16232d}}
.measurement{{min-width:145px;white-space:nowrap}}.measurement b{{font-variant-numeric:tabular-nums}}
.track{{height:7px;background:#273945;border-radius:3px;margin:9px 0 5px;position:relative}}
.track::after{{content:'';position:absolute;left:50%;top:-3px;height:13px;border-left:1px solid #e4edf3}}
.overflow{{position:absolute;right:0;top:-9px;color:white;font-size:17px}}
.delta{{color:#d0e1eb}}
.bar{{height:100%;display:block;border-radius:3px}}.bar.upstream{{background:var(--up)}}.bar.river-raptor{{background:var(--river)}}.bar.ghostty{{background:var(--ghost)}}
.good{{color:var(--river);min-width:100px}}.slow,.error{{color:#ffac9b;min-width:100px}}.good small,.slow small{{color:inherit}}
.tag{{background:#334450;padding:2px 5px;border-radius:4px;font-size:10px}}summary{{cursor:pointer;color:#b9d5e9;padding:9px 0}}
details{{border-bottom:1px solid #2e424f;padding:3px 0}}.case-details{{font-size:12px;min-width:220px;border:0}}
.case-details[open]{{min-width:420px;max-width:650px}}.samples td{{font-size:12px;min-width:140px}}
.qualification{{font-size:11px;color:#f3be77;max-width:360px}}
pre{{white-space:pre-wrap;padding:12px;background:#17242e;border-radius:5px}}
dl{{display:grid;grid-template-columns:180px minmax(0,1fr);gap:6px 16px;font-size:12px}}dd{{margin:0;overflow-wrap:anywhere}}
.note{{border-left:3px solid #57788e;padding-left:14px;color:#b7c8d4;max-width:1100px}}
@media(max-width:700px){{main{{padding:18px}}.engines{{grid-template-columns:1fr}}dl{{grid-template-columns:1fr}}}}
</style><main><p class="muted">{esc(report["id"])}</p><h1>Terminal benchmark</h1>
<p>Status: <b>{esc(report["status"])}</b> · <a href="report.json">Download machine-readable JSON</a></p>{error}
<div class="stats"><span><b>{completed}</b> completed cases</span><span><b>{options.get("samples", "—")}</b> samples / engine</span>
<span><b>{options.get("warmups", "—")}</b> warmups / engine</span><span><b>{esc(report.get("cpu", "—"))}</b> pinned CPU</span></div>
<div class="engines">{"".join(cards)}</div>
<h2>Timing comparisons</h2><p>Median time per operation. <b>Lower is faster.</b> River-raptor is always at 50% of the bar.
Other bars show time relative to it: +20% time reaches 60% of the bar; −20% reaches 40%.
An arrow marks costs above 2×, with the exact percentage shown below. Ranges show minimum–maximum.
Expand a case for every sample and its setup.</p>
<p class="note">Speedups compare the baseline with river-raptor. Native workloads differ; read each case’s comparison limits.
These microbenchmarks do not establish semantic equivalence or application performance. Noop controls are hidden by default.</p>
<div class="toolbar"><input id="search" placeholder="Filter cases…" aria-label="Filter cases">
<label><input id="controls" type="checkbox"> Show controls</label>
<select id="sort" aria-label="Sort cases"><option value="name">Sort by case</option>
<option value="ghostty">Largest gain vs Ghostty</option><option value="slower">Smallest gain vs Ghostty</option>
<option value="upstream">Largest gain vs upstream</option></select><span id="visible" class="muted"></span></div>
<div class="scroll"><table><thead><tr><th>Case / details</th><th>River-raptor</th><th>Upstream</th><th>Ghostty</th>
<th>vs upstream</th><th>vs Ghostty</th></tr></thead><tbody id="cases">{"".join(rows)}</tbody></table></div>
{provenance(report)}</main><script>
function filter() {{
const query=document.getElementById('search').value.toLowerCase();
const controls=document.getElementById('controls').checked;
const sort=document.getElementById('sort').value;
const rows=[...document.querySelectorAll('#cases > tr')];
rows.sort((a,b)=>sort==='name'?a.dataset.name.localeCompare(b.dataset.name):
sort==='slower'?Number(a.dataset.ghostty)-Number(b.dataset.ghostty):Number(b.dataset[sort])-Number(a.dataset[sort]));
for(const row of rows) {{row.hidden=!row.dataset.name.toLowerCase().includes(query)||(row.dataset.control==='true'&&!controls);
document.getElementById('cases').appendChild(row);}}
document.getElementById('visible').textContent=rows.filter(r=>!r.hidden).length+' cases shown';
}}
document.getElementById('search').oninput=filter;document.getElementById('controls').onchange=filter;
document.getElementById('sort').onchange=filter;filter();
</script></html>'''


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("json", type=Path, help="Existing report.json to render without rerunning benchmarks")
    args = parser.parse_args()
    output = args.json.with_suffix(".html")
    output.write_text(render(json.loads(args.json.read_text()), args.json.resolve().parent))
    print(output)
