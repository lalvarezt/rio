// Deterministic PTY workload. Rio terminates this child when the capture ends.
const fs = require('node:fs');
const [scenario = 'cached', metadata] = process.argv.slice(2);
const columns = process.stdout.columns || 80;
const rows = process.stdout.rows || 24;
let tick = 0;

function paint() {
  const token = scenario === 'unicode'
    ? (tick % 2 ? '日本語 e\u0301 Ω Ж ' : '中文語 a\u0308 λ Д ')
    : (tick % 2 ? '0123456789 abcdefghijklmnopqrstuvwxyz ' : '9876543210 ABCDEFGHIJKLMNOPQRSTUVWXYZ ');
  // Both Unicode tokens occupy 13 terminal cells, including spaces.
  const width = scenario === 'unicode' ? 13 : token.length;
  const line = token.repeat(Math.floor((columns - 1) / width));
  let output = '\x1b[?2026h\x1b[H';
  for (let row = 0; row < rows; row++) {
    output += `\x1b[${row + 1};1H\x1b[${31 + row % 6}m${line}\x1b[0m\x1b[K`;
  }
  output += '\x1b[?2026l';
  process.stdout.write(output);
  tick++;
}

process.stdout.write('\x1b[2J\x1b[?25l');
paint();
if (metadata) fs.writeFileSync(metadata, JSON.stringify({ scenario, columns, rows, pid: process.pid }));
process.stdout.write('\x1b]2;RIO_PROFILE_READY\x07');
if (scenario === 'ascii' || scenario === 'unicode') {
  setInterval(paint, 16);
} else {
  setInterval(() => {}, 1000);
}
