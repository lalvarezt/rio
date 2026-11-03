// Usage: node summarize.cjs SUMMARY.json CAPTURE_DIRECTORY...
const fs = require('node:fs');
const path = require('node:path');
const [destination, ...directories] = process.argv.slice(2);
if (!destination || !directories.length) throw new Error('Usage: node summarize.cjs SUMMARY.json CAPTURE_DIRECTORY...');
const median = values => {
  const sorted = [...values].sort((a, b) => a - b);
  return (sorted[Math.floor((sorted.length - 1) / 2)] + sorted[Math.floor(sorted.length / 2)]) / 2;
};
const summary = [];
for (const directory of directories) {
  const manifest = JSON.parse(fs.readFileSync(path.join(directory, 'manifest.json'), 'utf8'));
  const captures = [];
  for (let run = 1; run <= manifest.repeats; run++) {
    const stem = path.join(directory, `${manifest.scenario}-${run}`);
    const capture = JSON.parse(fs.readFileSync(stem + '.json', 'utf8'));
    const workload = JSON.parse(fs.readFileSync(stem + '.workload.json', 'utf8'));
    const measured = capture.target_frames - capture.warmup_frames;
    if (capture.frames !== capture.target_frames || capture.ready_tabs !== capture.target_tabs || measured < 1) {
      throw new Error(`Incomplete capture: ${stem}`);
    }
    const metrics = capture.steady_metrics;
    for (const name of ['frame.screen', 'frame.renderer', 'frame.grid', 'frame.present', 'gpu.acquire', 'gpu.upload', 'gpu.encode', 'gpu.submit', 'gpu.present']) {
      if (metrics[name]?.count !== measured) throw new Error(`Unbalanced warmed ${name} samples: ${stem}`);
    }
    const perFrame = name => (metrics[name]?.total_ms || 0) / measured;
    const stages = {
      snapshot: perFrame('frame.snapshot'),
      renderer_other: perFrame('frame.renderer') - perFrame('frame.snapshot'),
      grid: perFrame('frame.grid'),
      screen_other: perFrame('frame.screen') - perFrame('frame.renderer') - perFrame('frame.grid') - perFrame('frame.present'),
      prepare: perFrame('gpu.prepare'),
      acquire: perFrame('gpu.acquire'),
      upload: perFrame('gpu.upload'),
      encode: perFrame('gpu.encode'),
      submit: perFrame('gpu.submit'),
      present: perFrame('gpu.present'),
      present_other: perFrame('frame.present') - ['gpu.prepare', 'gpu.acquire', 'gpu.upload', 'gpu.encode', 'gpu.submit', 'gpu.present'].reduce((sum, name) => sum + perFrame(name), 0),
    };
    for (const [name, value] of Object.entries(stages)) {
      if (value < -0.001) throw new Error(`Invalid stage nesting, ${name}: ${stem}`);
    }
    captures.push({ file: path.resolve(stem + '.json'), measured, workload, metrics, stages });
  }
  const names = [...new Set(captures.flatMap(capture => Object.keys(capture.metrics)))];
  const metrics = Object.fromEntries(names.map(name => {
    const values = captures.map(capture => capture.metrics[name]).filter(Boolean);
    return [name, {
      median_ms: median(values.map(value => value.median_ms)),
      p95_ms: median(values.map(value => value.p95_ms)),
      mean_ms: median(values.map(value => value.total_ms / value.count)),
      median_range_ms: [Math.min(...values.map(value => value.median_ms)), Math.max(...values.map(value => value.median_ms))],
    }];
  }));
  const stages = Object.fromEntries(Object.keys(captures[0].stages).map(name => [name, median(captures.map(capture => capture.stages[name]))]));
  summary.push({ manifest, dimensions: captures.map(capture => capture.workload), metrics, exclusive_mean_ms_per_frame: stages, captures: captures.map(({ file, measured }) => ({ file, measured })) });
}
fs.writeFileSync(destination, JSON.stringify(summary, null, 2) + '\n');
console.log(`${path.resolve(destination)}: ${summary.length} scenarios validated`);
