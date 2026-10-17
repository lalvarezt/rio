# Windows background rendering pass

Measured on 2026-10-17 on BASLTOFF0370: Windows 11 build 22631, Intel
Core i7-1370P, NVIDIA RTX A500 Laptop GPU, NVIDIA driver 596.71, WGPU Vulkan.

The change skips color-space conversion for background cells with zero alpha.
The check follows the cursor overlay, so a visible cursor still renders over
a transparent cell. Nonzero alpha, including 1/255, takes the existing path.
This adds no buffers, caches, allocations, or persistent profiling code.

## GPU measurements

A temporary offscreen Windows executable used the production WGSL background
shader, fullscreen triangle, storage/uniform layout, BGRA8 target, and
premultiplied blending. It compiled both shader variants in the same process
and alternated their execution order. WGPU timestamp queries measured GPU
execution independently of CPU scheduling, presentation, and DWM composition.

Each sample rendered 16 draws at 1536 × 960 pixels and divided the GPU duration
by 16. Each case had 10 warmup pairs followed by 30 measured pairs. Attachment
clear/store overhead is included and amortized across the draws. These are
background-pass measurements, not whole-frame latency or presented FPS.

Second-run medians, using the default sRGB input color space:

| Background | Before (µs/draw) | After (µs/draw) | GPU time change |
|---|---:|---:|---:|
| One colored cell per row | 63.360 | 30.336 | −52.1% |
| Cursor over transparent cells | 56.640 | 27.072 | −52.2% |
| Transparent cells with nonzero RGB | 55.936 | 26.624 | −52.4% |
| Alternating colored/transparent cells | 67.008 | 56.512 | −15.7% |
| Fully opaque | 79.744 | 79.872 | +0.2% |
| Alpha 1/255 throughout | 73.280 | 73.472 | +0.3% |
| Translucent cursor over transparent cells | 55.808 | 26.624 | −52.3% |

An independent first run measured 52.1–52.3% less GPU time for the corresponding
sparse and cursor cases. Fully opaque output was unchanged within 0.2%.
The second run also covered Display P3 and Rec.2020 inputs. All 21 case/color
space combinations produced identical before/after framebuffer hashes over
the complete image. This includes visible and translucent cursor overlays,
mixed cell opacity, nonzero RGB under zero alpha, and faint visible cells.

The renderer already skips the entire background pass when both the cell
backgrounds and cursor are absent. That case gets no additional benefit.
This optimization helps when some visible content requires the pass but
much of its area is transparent. It does not accelerate text shading,
CPU row construction, window blur, or composition.

## ASCII recheck

The previous validation reported a 4.5% plain-ASCII slowdown. A fresh set of
11 alternating CPU-pinned pairs, using the same retained before/after
executables, measured 300,887 versus 302,304 ns per 262,136-byte batch: a 0.47%
increase. The earlier slowdown did not reproduce at comparable magnitude.
No compiler-layout workaround was added on this evidence. The shader change
does not modify terminal input processing.

## Integrated validation and artifacts

The Windows candidate included the prepared Linux-pass changes plus this
WGSL change. The fast optimized Windows build passed with LTO disabled and
16 codegen units. All 128 terminal-state equivalence cases matched the
previous prepared candidate. Desktop-session smoke captures completed 180
frames with one ready cmd tab and 182 frames with four ready cmd tabs.
These captures check integration, not a frame-time improvement.

Temporary harnesses and raw results are retained under the ignored local
`target/windows-linux-pass/` directory:

- `gpu-wgpu.rs`, `gpu-round1.jsonl`, `gpu-round2.jsonl`
- `ascii-recheck.jsonl`
- `equivalence-after.jsonl`, `equivalence-candidate.jsonl`
- `candidate-smoke.jsonl`

Windows artifacts remain in `D:\git-repos\rio\target\windows-linux-pass`.
The retained `rio-gpu-probe.exe` accepts a baseline shader path; run from the
repository with `target\windows-linux-pass\grid-before.wgsl` as its argument
to recreate the shader comparison. The `rio-profile-candidate.exe` executable
is an auto-exiting profiling application, not an installed Rio replacement.
The Windows checkout is restored after validation; no Windows commit,
user configuration change, or installed application replacement is needed.
