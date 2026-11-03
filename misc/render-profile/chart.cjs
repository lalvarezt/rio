// Usage: node chart.cjs BASELINE.json OUTPUT.html
const fs = require('node:fs');
const [source, destination] = process.argv.slice(2);
if (!source || !destination) throw new Error('Usage: node chart.cjs BASELINE.json OUTPUT.html');
const data = JSON.parse(fs.readFileSync(source, 'utf8'));
const labels = { cached: 'Cached redraw', full: 'Full damage', ascii: 'Changing ASCII', unicode: 'Changing Unicode' };
const groups = [
  ['Snapshot + chrome', s => s.snapshot + s.renderer_other],
  ['Grid preparation', s => s.grid],
  ['Prepare + upload + encode', s => s.prepare + s.upload + s.encode],
  ['Submit + present', s => s.submit + s.present],
  ['Other', s => s.screen_other + s.present_other],
];
const rows = data.map(row => ({ label: labels[row.manifest.scenario], values: groups.map(([, value]) => value(row.exclusive_mean_ms_per_frame) * 1000) }));
const maximum = Math.ceil(Math.max(...rows.map(row => row.values.reduce((a, b) => a + b, 0))) / 50) * 50;
const colors = groups.map((_, index) => `var(--chart-${index + 1},${['#5887df', '#d99337', '#54a58a', '#ad73ca', '#7e8794'][index]})`);
let bars = '';
for (let index = 0; index < rows.length; index++) {
  const row = rows[index];
  const y = 20 + index * 53;
  bars += `<text x="0" y="${y + 18}">${row.label}</text>`;
  let x = 155;
  for (let segment = 0; segment < row.values.length; segment++) {
    const width = row.values[segment] / maximum * 470;
    bars += `<rect x="${x}" y="${y}" width="${width}" height="27" fill="${colors[segment]}"><title>${row.label}, ${groups[segment][0]}: ${row.values[segment].toFixed(1)} µs</title></rect>`;
    x += width;
  }
  bars += `<text x="${x + 6}" y="${y + 18}">${row.values.reduce((a, b) => a + b, 0).toFixed(0)} µs</text>`;
}
let ticks = '';
for (let value = 0; value <= maximum; value += 50) {
  const x = 155 + value / maximum * 470;
  ticks += `<line x1="${x}" y1="10" x2="${x}" y2="218" stroke="var(--border,#ddd)"/><text x="${x}" y="240" text-anchor="middle">${value}</text>`;
}
const html = `<!doctype html>
<html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Rendering pipeline profile</title>
<style>
body{margin:0;color:var(--foreground,#222);font-family:var(--font-sans,system-ui);font-size:14px}h2{font-size:18px;margin:4px 0 8px}p{line-height:1.5;margin:8px 0;color:var(--muted-foreground,#666)}svg{width:100%;height:260px;display:block}svg text{fill:currentColor;font-size:12px}.legend{display:flex;gap:8px 16px;flex-wrap:wrap;font-size:12px}.key{display:inline-block;width:10px;height:10px;margin-right:5px}.note{font-size:12px}
</style></head><body>
<h2>Grid preparation is the main CPU target</h2>
<p>Native Vulkan, Intel Iris Xe, 155 × 42 cells. Three runs per workload, 300 measured frames per run after warmup.</p>
<svg viewBox="0 0 700 252" role="img" aria-label="Non-acquire main-thread time by rendering stage, in microseconds">${ticks}${bars}</svg>
<div class="legend">${groups.map(([name], i) => `<span><i class="key" style="background:${colors[i]}"></i>${name}</span>`).join('')}</div>
<p>Swapchain acquisition accounts for approximately 98–99% of each 16.65 ms frame under the virtual display's 60 Hz pacing. The chart excludes that wait to expose the smaller stages.</p>
<p class="note">Bars show medians across runs of exclusive mean wall times. They include instrumentation and driver calls. These are not GPU execution times. ASCII and Unicode use different glyph mixes.</p>
</body></html>`;
fs.writeFileSync(destination, html);
console.log(destination);
