# Renderer selection on the Windows laptop

Measured on 2026-10-19, from `96e6157459141d4d91890c027726d5b192457a29`.

Keep **NVIDIA Vulkan** on BASLTOFF0370. The existing Windows configuration
already selects it. It provides correct transparency and low frame-interval
tails on this laptop. No application source, user configuration, power policy,
or global GPU preference was changed by this comparison.

The machine has an i7-1370P, NVIDIA RTX A500 Laptop GPU (596.71), and Intel
Iris Xe (32.0.101.7088). Tests ran in the unlocked console session at
1920 × 1080, 60 Hz, DPI 96, with the Balanced power plan.

## Matched comparisons

Each row summarizes three repetitions. Startup uses one cmd tab and waits for
a submitted frame after the shell-ready marker. Dense redraws use four cmd
tabs and 240 full redraws, with Consolas 14 in a 1440 × 900 client area.
Opaque means opacity 1 and no blur; translucent means opacity 0.5 and blur.

All values are milliseconds. CPU is total Rio process CPU time for the entire
dense capture, including initialization and shutdown; it excludes child shells.
Frame p95 is the median of each run's frame-interval p95.

| Backend and adapter | Startup median (range) | Opaque CPU | Opaque frame p95 | Translucent CPU | Translucent frame p95 |
| --- | ---: | ---: | ---: | ---: | ---: |
| Vulkan, NVIDIA | 2226 (1535–2316) | 312.5 | 2.89 | 437.5 | 2.42 |
| DX12, NVIDIA | 2261 (1959–2783) | 343.8 | 1.98 | 546.9 | 2.36 |
| OpenGL, Intel | 723 (645–924) | 234.4 | 18.43 | 250.0 | 18.69 |
| Vulkan, Intel only | 2100 (1601–2781) | 296.9 | 17.64 | 234.4 | 17.90 |
| Vulkan, NVIDIA driver only | 1824 (1628–2058) | 328.1 | 2.30 | 421.9 | 2.60 |
| OpenGL, NVIDIA | 1884 (1782–2723) | 1078.1 | 17.26 | 937.5 | 17.24 |
| Automatic, selected NVIDIA Vulkan | 3000 (2529–4972) | 421.9 | 3.05 | 500.0 | 3.14 |

These are CPU wall-time intervals and driver waits, not GPU timestamps or
input-to-display latency. NVIDIA Vulkan and DX12 use Mailbox presentation;
the Intel paths and OpenGL use FIFO. Their different presentation policies
affect cadence and total CPU work. This capacity workload does not imply
that animations should run above the display refresh rate.

Intel OpenGL starts fastest but does not render the requested transparency
correctly. DX12's slightly lower opaque interval tail also does not make it a
replacement for the current translucent Vulkan configuration.

## Opacity validation

Separate foreground screen captures used two controlled backgrounds,
RGB (32, 96, 168) and (168, 96, 32). Two clear terminal-body patches were
sampled after visible text appeared. Eighteen of 21 combinations produced
foreground captures; the three Intel Vulkan combinations failed foreground
acquisition on both attempts. That is a capture limitation, not evidence of
a driver crash or a measured opacity result.

| Path | Opacity 1 | Opacity 0.5 with blur | Opacity 0 without blur |
| --- | --- | --- | --- |
| NVIDIA Vulkan, including driver-only and automatic | Opaque | Background changes visible | Exact background RGB |
| NVIDIA DX12 | Opaque | Remains opaque | Remains opaque |
| Intel OpenGL | Opaque | Background visible with tint | Background RGB plus Rio's (15, 13, 14) clear color |
| NVIDIA OpenGL | Opaque | Remains opaque | Remains opaque |
| Intel Vulkan | Capture unavailable | Capture unavailable | Capture unavailable |

NVIDIA Vulkan advertises and selects `PreMultiplied` surface alpha.
DX12 and both OpenGL drivers advertise only `Opaque`; Intel Vulkan advertises
`Opaque` and `Inherit`. Rio already favors an alpha-capable hardware adapter
when transparency is requested. Automatic selection chose NVIDIA Vulkan in
both diagnostic styles.

The Intel OpenGL tint is consistent with straight clear-color RGB reaching a
compositor that still uses alpha, despite the advertised opaque surface.
Premultiplying every Windows OpenGL clear would also change the NVIDIA opaque
fallback, so this experiment does not apply that workaround. The selected
Vulkan path passes the zero-opacity check. Uniform backdrops validate alpha
composition; they do not quantify the operating system's blur radius or tint.

## Startup filtering and residency

The three-run startup medians are noisy. A follow-up alternated six pairs of
default Vulkan and NVIDIA-only Vulkan with the process-local filter
`VK_LOADER_DRIVERS_SELECT=nv-vk64.json`. Their medians were 1735 and 1810 ms.
NVIDIA-only won two pairs and lost four; paired savings were 1295, −13, −15,
−161, 11, and −123 ms. The first-pair outlier does not establish a repeatable
filtering benefit. Leave the driver filter unset.

Keeping another idle NVIDIA Vulkan Rio context alive helped every one of
three paired startup comparisons:

| Startup condition | Shell-ready median | GPU instance median | GPU context median |
| --- | ---: | ---: | ---: |
| No benchmark keeper | 2903 | 1998 | 2300 |
| Idle NVIDIA context resident | 1172 | 302 | 590 |

Paired shell-ready savings were 1086, 1786, and 1733 ms. The median startup
fell by 60%. The no-keeper condition waited five seconds; it does not prove a
physically cold GPU. The keeper remained alive through each subject capture
and was closed afterward. No persistent helper remains.

This establishes a graphics-residency benefit, not the effect of a changed
NVIDIA power setting. Fresh processes still pay for their graphics resources.
Reusing an existing Rio process can also reuse the branch's cached GPU instance.
Neither a clock policy nor a GPU preference retains a terminated process's
graphics objects.

NVIDIA's [power-management reference](https://www.nvidia.com/content/Control-Panel-Help/vLatest/en-us/mergedProjects/3D%20Settings/Manage_3D_Settings_%28reference%29.htm)
describes **Prefer maximum performance** while an application runs. This
comparison does not benchmark that policy. Linux persistence mode addresses
a different driver lifecycle; see NVIDIA's
[driver-persistence overview](https://docs.nvidia.com/deploy/driver-persistence/overview.html).

## Measurement controls and priority correction

The authoritative dataset contains **81 timing captures**: 63 main comparisons,
12 driver-filter follow-ups, and six residency captures. Main comparisons
rotate order forward, backward, then by three positions. Adapter identities
and alpha capabilities were checked in 14 separate diagnostic launches.
No builds ran during timing; there was no reboot or shader-cache purge.

All authoritative jobs launch through Task Scheduler COM with priority 4 in
the user's interactive session. The driver verifies Normal CPU class (32),
Normal memory priority (5), and Normal I/O priority (2) before measurement.
Earlier temporary jobs used task priority 7 and then reset only CPU priority.
A direct probe found that memory and I/O priority remained low. Those initial
timings are archived but excluded from these results. The
[VM report](windows-vm-renderer-selection.md) now states the same limitation
for its earlier launcher; its I/O and memory priorities were not measured.

Microsoft documents priorities 4–6 for interactive tasks and 7–8 for background
tasks in [TaskSettings.Priority](https://learn.microsoft.com/en-us/windows/win32/taskschd/tasksettings-priority).
Merely reporting Normal CPU class was insufficient validation of this launcher.

Every timing capture satisfies its shell/tab/frame targets. The same executable
was used throughout, built once as an optimized Cargo benchmark artifact
(optimization 3, LTO off, 16 codegen units). Temporary benchmark-target and
adapter/alpha logging changes were restored after building. Executable SHA-256:

`fb29d8c76fdc70687596f6a1898edc97ad8f6ebc7beca6b367fabea249478a66`

NVIDIA OpenGL was selected with an isolated per-executable Windows GPU
preference, restored to its original absent value. Driver-filter cases used
process-local variables and verified the adapter. The user's Vulkan/opacity
0.5/blur configuration was preserved byte for byte. Cleanup verified no benchmark
processes remained, removed nine temporary tasks, and confirmed clean Windows
sources.

## Evidence and reproduction

[Machine-readable results](windows-renderer-selection.json) include individual
timings, aggregates, adapter logs, pixel samples, scheduler probes, and cleanup.
The full executable, exact temporary scripts, configs, reports, logs, captures,
and machine metadata are archived on both machines at:

`target/backend-selection-laptop-20261019.zip`

On Windows this is under `D:\git-repos\rio`. Archive SHA-256:

`fc573a611a6cfdf89db07c05bf16a0e0dc2d78cb2d0c96e191a65a67ccadf256`

The archive's `normal.py`, `filter_pairs.py`, and `resident.py` define the
authoritative workloads. Their `start-*.ps1` launchers use interactive task
priority 4. Build settings and the temporary diagnostic patch are in
`build.json`; `machine.json` records the interactive display. General capture
instructions and metric definitions are in the [profiling guide](README.md).
The earlier 2026-10-15 laptop experiment remains available in Git history;
its different workload is not combined with this dataset.
