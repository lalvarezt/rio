const fs = require('fs');
const path = require('path');
const {parseArgs} = require('util');
const {values} = parseArgs({options: {
  'results-dir': {type: 'string', default: path.resolve(__dirname, '../../bench-results')},
  'captures-dir': {type: 'string', default: path.resolve(__dirname, '../../target')},
  output: {type: 'string'}, help: {type: 'boolean', short: 'h'},
}});
if (values.help) {
  console.log('Usage: node timeline-chart.cjs [--results-dir DIR] [--captures-dir DIR] [--output FILE]');
  process.exit(0);
}
const root = path.resolve(values['results-dir']);
const output = path.resolve(values.output || path.join(root, 'timing-history.html'));
const compare = (a, b) => a < b ? -1 : a > b ? 1 : 0;
const json = value => JSON.stringify(value).replace(/</g, '\\u003c');
const runs = fs.readdirSync(root).filter(x => /^20/.test(x) && fs.existsSync(path.join(root, x, 'report.json'))).sort(compare).map(id => {
  const j = JSON.parse(fs.readFileSync(path.join(root, id, 'report.json'), 'utf8'));
  return {id, date:j.started_utc, commit:j.engines?.['river-raptor']?.commit?.slice(0,8)||'unknown', cpu:j.cpu,
    samples:j.options.samples, native:!!j.options.native, bytes:j.options.bytes,
    rows:j.results.map(r=>({name:r.case,control:r.control,notes:r.notes,operations:r.operations,ns:r.median_ns?.['river-raptor'],upstream:r.median_ns?.upstream}))};
});
const html = `<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><style>
body{font:14px var(--font-sans,system-ui);color:var(--foreground,#ddd);margin:0}h2{font-size:24px;margin:0 0 8px}p{line-height:1.55;color:var(--muted-foreground,#999)}select,input,button{background:var(--secondary,#242830);color:inherit;border:1px solid var(--border,#555);padding:8px;border-radius:6px}label{display:inline-block;margin:6px 10px 6px 0}.panel{padding:16px;background:var(--card,#171b22);border-radius:10px;margin:16px 0}.good{color:#32bd95}.bad{color:#f3797b}svg{width:100%;height:280px}table{border-collapse:collapse;width:100%;font-size:12px}td,th{text-align:left;padding:8px 5px;border-bottom:1px solid var(--border,#444)}td:first-child{overflow-wrap:anywhere}th{color:var(--muted-foreground,#999)}.scroll{overflow:auto;max-height:620px}.tiny{font-size:12px}#heat td{padding:3px;min-width:13px;text-align:center}#heat td:first-child{min-width:190px;text-align:left}a{color:inherit} .legend{display:flex;gap:18px;flex-wrap:wrap}</style></head><body>
<h2>Benchmark timing history</h2><p>Rio river-raptor across recorded suite runs. Lower elapsed time is better.</p>
<label>Series <select id="series"><option value="portable">Portable, 1 MiB / 16 MiB inputs</option><option value="small">Initial small-input run</option><option value="native">Native code generation</option></select></label>
<label>Baseline <select id="base"></select></label><label>Case <select id="case"></select></label>
<div class="panel"><div id="headline"></div><svg id="plot" viewBox="0 0 700 280" role="img" aria-label="Selected benchmark timing across recorded runs"></svg><div id="detail" class="tiny"></div></div>
<div id="patterns" class="panel"></div>
<h3>Every case over time</h3><p class="tiny">Each cell is elapsed-time change from the selected baseline. Green is faster, red is slower; hover for timing and run. Gray means missing. Columns are recorded runs, equally spaced. Click a case to inspect it above.</p><div class="legend tiny"><span class="good">Green ≤ −5%</span><span>Muted within ±5%</span><span class="bad">Red ≥ +5%</span></div><div class="scroll"><table id="heat"></table></div>
<h3>Latest run compared with baseline</h3><label>Filter <input id="filter" placeholder="Search benchmark names"></label><div class="scroll"><table><thead><tr><th>Case</th><th>Baseline</th><th>Latest</th><th>Time change</th><th>Latest vs previous</th></tr></thead><tbody id="rows"></tbody></table></div>
<details><summary>Run inventory and interpretation</summary><p>Percent change = 100 × (new median / baseline median − 1). The ±5% color threshold is descriptive, not a significance test. Measurements are medians from 1–9 samples. No aggregate performance score is used. Controls remain visible and marked.</p><p>The portable series starts with the first full-size run. CPU pinning, sample counts, harness code and resize adapters changed across runs; observed changes cannot all be attributed to code. Native and initial smaller-input measurements have separate series. Partial runs contribute only their measured cases. Failed runs with no results appear only in the inventory. Exploratory paired experiments, profiles, UI audits, and other benchmark families are excluded from this suite timeline because they use different formats and workloads. Ghostty comparisons are excluded because per-case work differs.</p><div class="scroll"><table id="inventory"></table></div></details>
<script>const all=${json(runs)};
const $=id=>document.getElementById(id),fmt=n=>n==null?'—':n>=1e6?(n/1e6).toFixed(2)+' ms':n>=1e3?(n/1e3).toFixed(2)+' µs':n.toFixed(0)+' ns';const pct=(n,b)=>n>0&&b>0?100*(n/b-1):null;const pf=n=>n==null?'—':(n>0?'+':'')+n.toFixed(1)+'%';const cls=n=>n<-5?'good':n>5?'bad':'';let runs=[];const row=(r,n)=>r?.rows.find(x=>x.name===n);const esc=s=>String(s).replace(/[&<>"']/g,c=>({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[c]));const date=r=>r.date.slice(5,10)+' '+r.date.slice(11,16);
function setup(){runs=all.filter(r=>r.rows.length&&($('series').value==='native'?r.native:$('series').value==='small'?r.bytes<1048576:!r.native&&r.bytes===1048576));$('base').innerHTML=runs.map((r,i)=>'<option value="'+i+'">'+date(r)+' UTC · '+r.commit+'</option>').join('');const names=[...new Set(runs.flatMap(r=>r.rows.map(x=>x.name)))].sort();$('case').innerHTML=names.map(n=>'<option>'+esc(n)+'</option>').join('');draw()}
function draw(){if(!runs.length){$('headline').textContent='No measured runs in this series.';$('plot').innerHTML='';$('rows').innerHTML='';$('heat').innerHTML='';$('patterns').innerHTML='';return;}const b=runs[+$('base').value],last=runs.at(-1),prev=runs.at(-2),name=$('case').value;const names=[...new Set(runs.flatMap(r=>r.rows.map(x=>x.name)))].sort();const data=names.map(n=>{const x=row(last,n),a=row(b,n);return {n,x,a,d:pct(x?.ns,a?.ns),recent:pct(x?.ns,row(prev,n)?.ns)}}).filter(x=>x.x);const ranked=data.filter(x=>x.d!=null&&!x.x.control).sort((a,b)=>a.d-b.d);const wins=ranked.filter(x=>x.d< -5),losses=ranked.filter(x=>x.d>5);$('headline').innerHTML='<b>'+esc(name)+'</b><p class="tiny">'+runs.length+' measured runs · baseline '+date(b)+' UTC · latest '+date(last)+' UTC</p>';
const vals=runs.map(r=>row(r,name)?.ns).filter(n=>n>0);const lo=Math.min(...vals)*.85,hi=Math.max(...vals)*1.08;const X=i=>65+i*610/Math.max(1,runs.length-1),Y=n=>235-(n-lo)/(hi-lo||1)*210;let svg='';for(let i=0;i<4;i++){let n=lo+(hi-lo)*i/3,y=Y(n);svg+='<line x1="65" x2="680" y1="'+y+'" y2="'+y+'" stroke="currentColor" opacity=".12"/><text x="0" y="'+(y+4)+'" fill="currentColor" font-size="11">'+fmt(n)+'</text>'}let points=[];runs.forEach((r,i)=>{const v=row(r,name)?.ns;if(v>0){points.push(X(i)+','+Y(v));svg+='<circle cx="'+X(i)+'" cy="'+Y(v)+'" r="4" fill="#52b5ed"><title>'+date(r)+' UTC | '+fmt(v)+' | '+r.commit+' | CPU '+r.cpu+'</title></circle>'}if(i===0||i===runs.length-1||r.cpu!==runs[i-1]?.cpu)svg+='<text x="'+X(i)+'" y="260" text-anchor="middle" fill="currentColor" font-size="10">'+date(r)+'</text>'});svg='<polyline points="'+points.join(' ')+'" fill="none" stroke="#52b5ed" stroke-width="2"/>'+svg;$('plot').innerHTML=svg;$('detail').textContent=(row(last,name)?.notes||'')+' CPU and harness changes may affect this trend.';
const describe=x=>esc(x.n)+' '+pf(x.d);$('patterns').innerHTML='<b>'+wins.length+' cases faster by >5%; '+losses.length+' slower by >5%</b><p>Largest reductions: '+ranked.slice(0,3).map(describe).join(' · ')+'.</p><p>Largest increases: '+ranked.filter(x=>x.d>0).slice(-3).reverse().map(describe).join(' · ')+'.</p><p class="tiny">Counts exclude controls. These are observed timing changes, without statistical significance claims.</p>';
$('heat').innerHTML='<thead><tr><th>Case / time change</th>'+runs.map((r,i)=>'<th title="'+r.id+'">'+(i+1)+'</th>').join('')+'</tr></thead><tbody>'+names.map(n=>'<tr><td><a href="#" data-name="'+esc(n)+'">'+esc(n)+'</a></td>'+runs.map(r=>{const v=row(r,n)?.ns,d=pct(v,row(b,n)?.ns);let color=d==null?'#666':d< -5?'#18856b':d>5?'#ae414b':'#626773';return '<td style="background:'+color+'" title="'+esc(n)+' | '+date(r)+' UTC | '+fmt(v)+' | '+pf(d)+'">'+(d==null?'·':d.toFixed(0))+'</td>'}).join('')+'</tr>').join('')+'</tbody>';
$('rows').innerHTML=data.filter(x=>x.n.includes($('filter').value)).sort((a,b)=>(b.d??-Infinity)-(a.d??-Infinity)).map(x=>'<tr><td>'+esc(x.n)+(x.x.control?' [control]':'')+'</td><td>'+fmt(x.a?.ns)+'</td><td>'+fmt(x.x.ns)+'</td><td class="'+cls(x.d)+'">'+pf(x.d)+'</td><td class="'+cls(x.recent)+'">'+pf(x.recent)+'</td></tr>').join('');document.querySelectorAll('[data-name]').forEach(a=>a.onclick=e=>{e.preventDefault();$('case').value=a.dataset.name;draw()});console.log('Rendered '+names.length+' cases across '+runs.length+' runs');}
$('inventory').innerHTML='<tr><th>Run UTC / commit</th><th>Cases</th><th>Samples</th><th>CPU</th><th>Build / input</th></tr>'+all.map(r=>'<tr><td title="'+r.id+'">'+date(r)+' / '+r.commit+'</td><td>'+r.rows.length+'</td><td>'+r.samples+'</td><td>'+r.cpu+'</td><td>'+(r.native?'native':'portable')+' / '+r.bytes+'</td></tr>').join('');$('series').onchange=setup;['base','case'].forEach(id=>$(id).onchange=draw);$('filter').oninput=draw;setup();</script></body></html>`;
const captures = [];
function filesIn(dir) {
  return fs.readdirSync(dir, {withFileTypes:true}).sort((a,b)=>compare(a.name,b.name)).flatMap(entry => {
    const file = path.join(dir, entry.name);
    return entry.isDirectory() ? filesIn(file) : entry.isFile() && /(?:comparison|replay|summary)\.json$/.test(entry.name) ? [file] : [];
  });
}
for (const directory of ['regressions-20261023', 'sugarloaf-profile']) {
  const dir = path.resolve(values['captures-dir'], directory);
  if (!fs.existsSync(dir)) continue;
  for (const source of filesIn(dir)) {
    const j = JSON.parse(fs.readFileSync(source, 'utf8'));
    const entries = Array.isArray(j) ? j.filter(r=>r.revision==='current').map(r=>({case:r.case,reference_ns:j.find(a=>a.case===r.case&&a.revision==='reference')?.median_ns,current_ns:r.median_ns})) : j.results || j.cases || j.common || [];
    const rows = entries.flatMap(r => {
      if (Number.isFinite(r.reference_median_ns) && Number.isFinite(r.current_median_ns)) return [{name:r.case, before:r.reference_median_ns, after:r.current_median_ns, pair:'reference → current', control:!!r.control}];
      if (Number.isFinite(r.reference_ns) && Number.isFinite(r.current_ns)) return [{name:r.case, before:r.reference_ns, after:r.current_ns, pair:'reference → current', control:!!r.control}];
      if (Number.isFinite(r.before_ns) && Number.isFinite(r.after_ns)) return [{name:r.case, before:r.before_ns, after:r.after_ns, pair:'before → after', control:!!r.control}];
      if (r.medians) {
        const keys = Object.keys(r.medians), first = keys[0];
        return keys.slice(1).map(k => ({name:r.case, before:r.medians[first], after:r.medians[k], pair:first+' → '+k, control:!!r.control}));
      }
      if (Number.isFinite(r.old_ns) && Number.isFinite(r.current_ns)) return [{name:r.case, before:r.old_ns, after:r.current_ns, pair:'old → current', control:!!r.control}];
      return [];
    });
    let metadata = {};
    const metadataPath = path.join(path.dirname(source), 'metadata.json');
    if (fs.existsSync(metadataPath)) metadata = JSON.parse(fs.readFileSync(metadataPath, 'utf8'));
    if (rows.length) captures.push({source:path.relative(path.resolve(values['captures-dir'], '..'),source).split(path.sep).join('/'), date:j.date||metadata.date||metadata.started_utc||'', cpu:j.cpu??metadata.cpu, samples:j.samples||j.pairs||j.samples_per_repeat||metadata.samples, binaries:j.binary_sha256||j.binaries||metadata.binaries_sha256, reference:j.reference_capture||j.reference, current:j.current_capture||j.capture, rows});
  }
}
captures.sort((a,b)=>compare(b.date,a.date)||compare(a.source,b.source));
const supplement = `<section><h3>Latest paired replays and renderer captures</h3><p>Stored paired captures. Ordered by embedded timestamps when available, then source path. Files without timestamps use source order. Each comparison uses its own measured reference. Experimental variants are included; inclusion does not mean the variant was retained.</p><label>Capture <select id="capture"></select></label><div id="captureMeta" class="tiny"></div><div id="captureSummary" class="panel"></div><div class="scroll"><table><thead><tr><th>Case / comparison</th><th>Reference</th><th>Variant</th><th>Time change</th><th>Relative change</th></tr></thead><tbody id="captureRows"></tbody></table></div></section><script>
document.addEventListener('DOMContentLoaded',()=>{
const captures=${json(captures)};
$('capture').innerHTML=captures.map((c,i)=>'<option value="'+i+'">'+esc(c.source)+'</option>').join('');
function drawCapture(){if(!captures.length){$('captureMeta').textContent='No paired captures found.';return;}const c=captures[+$('capture').value],rows=c.rows.map(r=>({...r,d:pct(r.after,r.before)})).sort((a,b)=>b.d-a.d);$('captureMeta').textContent=c.source+' | recorded '+(c.date||'timestamp unavailable')+' | '+(c.samples||'unspecified')+' samples/pairs | CPU '+(c.cpu??'unspecified')+' | '+JSON.stringify(c.binaries||{reference:c.reference,current:c.current});const gains=rows.filter(r=>r.d< -5&&!r.control),losses=rows.filter(r=>r.d>5&&!r.control);$('captureSummary').innerHTML='<b>'+gains.length+' comparisons faster by >5%; '+losses.length+' slower by >5%</b><p class="tiny">'+rows.length+' measured comparisons. Negative means faster. Bars use a fixed ±100% range and cap larger changes. The threshold is descriptive, without significance claims. Different variants can repeat the same case.</p>';$('captureRows').innerHTML=rows.map(r=>'<tr><td>'+esc(r.name)+'<br><small>'+esc(r.pair)+(r.control?' [control]':'')+'</small></td><td>'+fmt(r.before)+'</td><td>'+fmt(r.after)+'</td><td class="'+cls(r.d)+'">'+pf(r.d)+'</td><td><div style="position:relative;width:100px;height:14px;border-left:1px solid transparent;background:linear-gradient(90deg,transparent 49%,#888 50%,transparent 51%)"><div style="position:absolute;height:10px;top:2px;left:'+(r.d<0?50-Math.min(50,Math.abs(r.d)/2):50)+'%;width:'+Math.min(50,Math.abs(r.d)/2)+'%;background:'+(r.d<0?'#18856b':'#ae414b')+'"></div></div></td></tr>').join('');console.log('Additional captures: '+captures.length+'; selected comparisons: '+rows.length)}$('capture').onchange=drawCapture;drawCapture();</script>`;
fs.writeFileSync(output, html.replace('</body>', supplement.replace('</script>', '});</script>')+'</body>'));
console.log('Included '+captures.length+' additional paired and renderer captures');
console.log('Created '+output+' from '+runs.length+' suite records');
