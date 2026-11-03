// Run a capture on an isolated hardware-backed virtual Wayland output.
// Usage: node virtual.cjs OUTPUT BINARY [SCENARIO] [REPEATS] [--perf]
const fs = require('node:fs');
const path = require('node:path');
const cp = require('node:child_process');

async function main() {
  const [outputArg, binary, ...args] = process.argv.slice(2);
  if (!outputArg || !binary) throw new Error('Usage: node virtual.cjs OUTPUT BINARY [SCENARIO] [REPEATS] [--perf]');
  const output = path.resolve(outputArg);
  fs.mkdirSync(output, { recursive: true });
  const runtime = fs.mkdtempSync(path.join(output, 'runtime-'));
  fs.chmodSync(runtime, 0o700);
  const environment = {
    ...process.env,
    XDG_RUNTIME_DIR: runtime,
    XDG_CONFIG_HOME: path.join(output, 'desktop-config'),
    XDG_CACHE_HOME: path.join(output, 'cache'),
    WAYLAND_DISPLAY: 'rio-profile-wayland',
  };
  for (const key of ['DISPLAY', 'DBUS_SESSION_BUS_ADDRESS', 'WINIT_UNIX_BACKEND', 'KWIN_COMPOSE']) delete environment[key];
  const log = fs.openSync(path.join(output, 'compositor.log'), 'w');
  const compositor = cp.spawn('dbus-run-session', [
    '--', 'kwin_wayland', '--virtual', '--width', '1280', '--height', '800',
    '--socket', environment.WAYLAND_DISPLAY,
    '--no-lockscreen', '--no-global-shortcuts', '--no-kactivities',
  ], { env: environment, detached: true, stdio: ['ignore', log, log] });
  let spawnError;
  compositor.on('error', error => { spawnError = error; });
  const stop = () => {
    if (compositor.pid) {
      try { process.kill(-compositor.pid, 'SIGTERM'); } catch {}
    }
  };
  process.once('exit', stop);
  try {
    for (let attempt = 0; attempt < 200; attempt++) {
      if (spawnError) throw spawnError;
      if (compositor.exitCode !== null || compositor.signalCode !== null) throw new Error('Compositor exited; see compositor.log');
      if (fs.existsSync(path.join(runtime, environment.WAYLAND_DISPLAY))) break;
      await new Promise(resolve => setTimeout(resolve, 50));
    }
    if (!fs.existsSync(path.join(runtime, environment.WAYLAND_DISPLAY))) throw new Error('No compositor socket; see compositor.log');
    const capture = cp.spawn(process.execPath, [path.join(__dirname, 'run.cjs'), binary, output, ...args], {
      env: environment, stdio: 'inherit',
    });
    const status = await new Promise((resolve, reject) => {
      capture.once('error', reject);
      capture.once('exit', (code, signal) => signal ? reject(new Error(`Capture received ${signal}`)) : resolve(code));
    });
    process.exitCode = status;
  } finally {
    stop();
    process.removeListener('exit', stop);
    fs.closeSync(log);
    await new Promise(resolve => compositor.exitCode !== null || compositor.signalCode !== null ? resolve() : compositor.once('exit', resolve));
    fs.rmSync(runtime, { recursive: true, force: true });
  }
}

main().catch(error => { console.error(error.message); process.exitCode = 1; });
