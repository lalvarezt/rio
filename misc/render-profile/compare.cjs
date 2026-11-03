// Usage: node compare.cjs BEFORE.json AFTER.json OUTPUT_PREFIX
const fs = require('node:fs');
const [beforePath, afterPath, output] = process.argv.slice(2);
if (!beforePath || !afterPath || !output) throw new Error('Usage: node compare.cjs BEFORE.json AFTER.json OUTPUT_PREFIX');
const before = JSON.parse(fs.readFileSync(beforePath, 'utf8'));
const after = JSON.parse(fs.readFileSync(afterPath, 'utf8'));
const rows = before.map(baseline => {
  const scenario = baseline.manifest.scenario;
  const candidate = after.find(row => row.manifest.scenario === scenario);
  if (!candidate) throw new Error(`Missing candidate scenario: ${scenario}`);
  if ([...baseline.dimensions, ...candidate.dimensions].some(row => row.columns !== candidate.dimensions[0].columns || row.rows !== candidate.dimensions[0].rows)) {
    throw new Error(`Viewport dimensions differ: ${scenario}`);
  }
  const previous = baseline.metrics['frame.grid'];
  const current = candidate.metrics['frame.grid'];
  return {
    scenario, before: previous, after: current,
    reduction_percent: (1 - current.median_ms / previous.median_ms) * 100,
  };
});
fs.writeFileSync(output + '.json', JSON.stringify(rows, null, 2) + '\n');
const maximum = Math.ceil(Math.max(...rows.flatMap(row => [row.before.median_ms, row.after.median_ms])) * 1000 / 50) * 50;
const colors = ['var(--chart-1,#5887df)', 'var(--chart-2,#54a58a)'];
const labels = { cached: 'Cached redraw', full: 'Full damage', ascii: 'Changing ASCII', unicode: 'Changing Unicode' };
const separated = rows.every(row => row.after.median_range_ms[1] < row.before.median_range_ms[0]);
let bars = '';
for (let index = 0; index < rows.length; index++) {
  const row = rows[index];
  const y = 18 + index * 90;
  bars += `<text x="0" y="${y + 18}">${labels[row.scenario]}</text>`;
  for (const [offset, metric] of [row.before, row.after].entries()) {
    const value = metric.median_ms * 1000;
    const width = value / maximum * 430;
    bars += `<rect x="145" y="${y + offset * 30}" width="${width}" height="23" fill="${colors[offset]}"/><text x="${151 + width}" y="${y + offset * 30 + 16}">${value.toFixed(1)} µs</text>`;
  }
  bars += `<text x="145" y="${y + 76}">${row.reduction_percent.toFixed(1)}% less grid-preparation time</text>`;
}
const html = `<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Foreground color reuse</title><style>
body{margin:0;color:var(--foreground,#222);font:14px var(--font-sans,system-ui)}h2{font-size:18px;margin:4px 0 8px}p{line-height:1.5;color:var(--muted-foreground,#666);margin:8px 0}svg{display:block;width:100%;height:${rows.length * 100 + 10}px}svg text{fill:currentColor;font-size:12px}.legend{display:flex;gap:20px;font-size:12px}.key{display:inline-block;width:10px;height:10px;margin-right:5px}
</style></head><body><h2>Reusing foreground colors reduces grid preparation</h2>
<p>Native Vulkan, 155 × 42 cells. Three runs per workload, with 300 measured frames per run after warmup.</p>
<svg viewBox="0 0 700 ${rows.length * 90 + 10}" role="img" aria-label="Before and after grid-preparation median times in microseconds">${bars}</svg>
<div class="legend"><span><i class="key" style="background:${colors[0]}"></i>Before</span><span><i class="key" style="background:${colors[1]}"></i>Row-local color reuse</span></div>
<p>${separated ? 'The ranges across runs do not overlap.' : 'At least one workload has overlapping run ranges.'} Frame cadence remains near 16.65 ms under the virtual output's 60 Hz pacing. These measurements describe CPU grid preparation.</p>
</body></html>`;
fs.writeFileSync(output + '.html', html);
console.log(output + '.json\n' + output + '.html');
