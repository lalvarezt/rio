// Usage: node misc/render-profile/run.cjs BINARY OUTPUT [SCENARIO] [REPEATS]
// Use a hardware-backed Wayland/X11 session. Xvfb lacks DRI3 on many systems.
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');
const [binaryArg, outputArg, scenario = 'cached', repeatsArg = '3', ...flags] = process.argv.slice(2);
if (!binaryArg || !outputArg || !['cached', 'full', 'ascii', 'unicode'].includes(scenario)) {
  throw new Error('Usage: node run.cjs BINARY OUTPUT [cached|full|ascii|unicode] [REPEATS] [--perf]');
}
const binary = path.resolve(binaryArg);
const output = path.resolve(outputArg);
const repeats = Number(repeatsArg);
if (!Number.isInteger(repeats) || repeats < 1) throw new Error('REPEATS must be a positive integer');
if (flags.some(flag => flag !== '--perf')) throw new Error('Unknown capture option');
fs.mkdirSync(output, { recursive: true });
const config = path.join(output, 'config');
fs.mkdirSync(config, { recursive: true });
fs.writeFileSync(path.join(config, 'config.toml'), `confirm-before-quit = false
use-fork = false
[window]
width = 1280
height = 800
[fonts]
family = "DejaVu Sans Mono"
size = 14
[renderer]
backend = "vulkan"
[navigation]
hide-if-single = false
`);
const environment = {
  ...process.env,
  RIO_CONFIG_HOME: config,
  RIO_PROFILE_FRAMES: process.env.RIO_PROFILE_FRAMES || '360',
  RIO_PROFILE_SETTLE_MS: '0',
  RIO_PROFILE_WARMUP_FRAMES: process.env.RIO_PROFILE_WARMUP_FRAMES || '60',
};
if (!environment.WAYLAND_DISPLAY) environment.WINIT_UNIX_BACKEND = 'x11';
delete environment.RIO_PROFILE_FULL_REDRAW;
if (scenario === 'full') environment.RIO_PROFILE_FULL_REDRAW = '1';
const frameTarget = Number(environment.RIO_PROFILE_FRAMES);
const warmupFrames = Number(environment.RIO_PROFILE_WARMUP_FRAMES);
if (!Number.isInteger(frameTarget) || !Number.isInteger(warmupFrames) || warmupFrames < 0 || frameTarget <= warmupFrames) {
  throw new Error('RIO_PROFILE_FRAMES must exceed the nonnegative RIO_PROFILE_WARMUP_FRAMES');
}
const manifest = {
  binary, scenario, repeats, frameTarget, warmupFrames,
  date: new Date().toISOString(),
  revision: cp.execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim(),
  platform: process.platform, arch: process.arch, node: process.version,
  display: process.env.WAYLAND_DISPLAY ? 'Wayland' : process.env.DISPLAY ? 'X11' : 'Xvfb 1920x1200x24',
};
fs.writeFileSync(path.join(output, 'manifest.json'), JSON.stringify(manifest, null, 2));
for (let run = 1; run <= repeats; run++) {
  const stem = path.join(output, `${scenario}-${run}`);
  if (fs.existsSync(stem + '.json')) throw new Error(`Capture already exists: ${stem}.json`);
  const args = [binary, '-e', process.execPath, path.join(__dirname, 'workload.cjs'), scenario, stem + '.workload.json'];
  if (flags.includes('--perf')) args.unshift('perf', 'record', '-q', '--delay=1500', '-F', '1999', '-e', 'cycles:u', '--call-graph', 'dwarf,16384', '-o', stem + '.perf.data', '--');
  if (!process.env.DISPLAY && !process.env.WAYLAND_DISPLAY) args.unshift('xvfb-run', '-a', '-s', '-screen 0 1920x1200x24');
  const command = args.shift();
  const fd = fs.openSync(stem + '.log', 'w');
  let result;
  try {
    result = cp.spawnSync(command, args, {
      env: { ...environment, RIO_PROFILE_OUTPUT: stem + '.json' },
      stdio: ['ignore', fd, fd], timeout: 45000,
    });
  } finally {
    fs.closeSync(fd);
  }
  if (result.error || result.status !== 0) throw new Error(`Capture failed, see ${stem}.log: ${result.error || result.status}`);
  const report = JSON.parse(fs.readFileSync(stem + '.json', 'utf8'));
  if (report.ready_tabs !== report.target_tabs || report.frames < report.target_frames || report.shell_ready_ms === null) {
    throw new Error(`Incomplete capture: ${stem}.json`);
  }
  console.log(`${stem}.json: ${report.frames} frames, screen median ${report.metrics['frame.screen']?.median_ms} ms`);
}
