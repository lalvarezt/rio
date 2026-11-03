// Usage: node analyze-perf.cjs SUMMARY.json CPU_CAPTURE_DIRECTORY...
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');
const [destination, ...directories] = process.argv.slice(2);
if (!destination || !directories.length) throw new Error('Usage: node analyze-perf.cjs SUMMARY.json CPU_CAPTURE_DIRECTORY...');
const summaries = [];
for (const directory of directories) {
  const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'manifest.json'), 'utf8'));
  const data = path.join(directory, `${manifest.scenario}-1.perf.data`);
  const environment = { ...process.env, DEBUGINFOD_URLS: '' };
  const periods = cp.execFileSync('perf', [
    'script', '-G', '-F', 'comm,event,period', '-i', data,
  ], { encoding: 'utf8', env: environment, timeout: 60000, maxBuffer: 16 * 1024 * 1024 });
  const mainEvents = new Map();
  for (const line of periods.split('\n')) {
    const match = line.match(/^\s*rio-profile\s+(\d+)\s+(\S+):\s*$/);
    if (!match) continue;
    const event = mainEvents.get(match[2]) || { samples: 0, cycles: 0 };
    event.samples++;
    event.cycles += Number(match[1]);
    mainEvents.set(match[2], event);
  }
  const report = cp.execFileSync('perf', [
    'report', '--stdio', '--no-children', '--call-graph', 'none',
    '--percent-limit', '0', '--percentage', 'relative', '--field-separator', '\t', '-i', data,
    '--comms', 'rio-profile', '--sort', 'dso,symbol', '--show-nr-samples',
  ], { encoding: 'utf8', env: environment, timeout: 60000, maxBuffer: 16 * 1024 * 1024 });
  fs.writeFileSync(path.join(directory, 'self.txt'), report);
  let events = [];
  let event;
  for (const line of report.split('\n')) {
    let match = line.match(/^# Samples:\s+(\S+)\s+of event '([^']+)'/);
    if (match) {
      event = { event: match[2], samples: 0, cycles: 0, symbols: [] };
      events.push(event);
    }
    const fields = line.trim().split('\t').map(field => field.trim());
    if (event && /^\d+\.\d+%$/.test(fields[0]) && fields.length >= 4) {
      const symbol = { percent: Number(fields[0].slice(0, -1)), samples: Number(fields[1]), dso: fields[2], symbol: fields[3].replace(/^\[.\] /, '') };
      if (!Number.isFinite(symbol.samples)) throw new Error(`Invalid perf row: ${line}`);
      event.samples += symbol.samples;
      event.symbols.push(symbol);
    }
  }
  // Hybrid CPUs can record an event used only by filtered-out child threads.
  events = events.filter(event => event.samples > 0 || mainEvents.has(event.event));
  for (const event of events) {
    const observed = mainEvents.get(event.event);
    if (observed?.samples !== event.samples) throw new Error(`Sample count mismatch: ${data} ${event.event}`);
    event.cycles = observed.cycles;
    const percent = event.symbols.reduce((sum, symbol) => sum + symbol.percent, 0);
    if (Math.abs(percent - 100) > 0.5) throw new Error(`Incomplete symbol percentages: ${data} ${percent}`);
  }
  if (!events.length || events.some(event => !event.samples || !event.cycles)) throw new Error(`Empty perf report: ${data}`);
  const cycles = events.reduce((sum, event) => sum + event.cycles, 0);
  const symbols = new Map();
  for (const event of events) {
    for (const symbol of event.symbols) {
      const key = symbol.dso + '\t' + symbol.symbol;
      const entry = symbols.get(key) || { dso: symbol.dso, symbol: symbol.symbol, percent: 0, samples: 0 };
      entry.percent += symbol.percent * event.cycles / cycles;
      entry.samples += symbol.samples;
      symbols.set(key, entry);
    }
  }
  summaries.push({
    scenario: manifest.scenario,
    source: path.resolve(data),
    events: events.map(({ symbols, ...event }) => event),
    symbols: [...symbols.values()].sort((a, b) => b.percent - a.percent),
  });
}
fs.writeFileSync(destination, JSON.stringify(summaries, null, 2) + '\n');
console.log(`${path.resolve(destination)}: ${summaries.length} CPU profiles summarized`);
