# Windows VM renderer selection — 2026-10-19

**DX12 is the fastest tested wgpu path for opaque windows on this VM.** Rio's
Windows default, `Webgpu`, already selects it. Explicitly setting
`WGPU_BACKEND=dx12` also avoids initializing unused graphics APIs and reduces
startup time. This result describes software rendering in the tested RDP session.

The host is `basvsapp6`, Windows Server 2025 build 26100, with four virtual CPUs
(Xeon Gold 5415+), 24 GiB RAM, and the Balanced power plan. Tests ran as
`App_dj7sjk` in active RDP session 3. Windows lists VMware SVGA 3D driver 9.17.9.4,
but neither working wgpu path selected that adapter:

| Requested path | Selected renderer | Device type | Surface alpha modes |
| --- | --- | --- | --- |
| DX12 | Microsoft Basic Render Driver, 10.0.26100.33438 | CPU | Opaque |
| OpenGL | llvmpipe LLVM 13.0.1, Mesa 24.1.0 | CPU | Opaque |
| Automatic | DX12 / Microsoft Basic Render Driver | CPU | Opaque |
| Vulkan | Failed to create a surface | Unavailable | Unavailable |

These results cannot select the fastest hardware backend for the laptop or
establish VMware GPU performance. Both working wgpu surfaces expose only opaque
composition. The opacity/blur workload below measures a requested configuration;
it does not certify correct transparent desktop composition on this machine.

The source revision was `fd822427d6bf8bd6679e7c0fe677d9a7dbe5e7a4`. Setup installed
Rust 1.96.1, Python 3.14.7, MSVC 14.44, and Windows SDK 22621. One Cargo benchmark
executable served every case: optimization level 3, LTO disabled, 16 codegen units,
no debug information, and unwind panics. Compilation finished before measurement.
A temporary startup log recorded adapter identity and surface capabilities; it
was disabled during timing, and the source changes were restored after building.

The fixture used Consolas 14, a 1440 × 900 client area, the tab bar, and cmd shells.
Startup used one tab. Dense cases used four ready tabs and 240 complete redraws
of the same ASCII fixture. The driver rejected incomplete frame or shell
workloads. Opaque means opacity 1 with blur off; blur means requested opacity 0.5
with blur on. Processes ran at Normal priority without CPU affinity pinning.

The initial comparison used three repetitions with rotating API order. Automatic
selection and the experimental CPU renderer were measured in a separate followup,
alternating their order. Values below are medians across three runs. Frame p95 is
the median of each run's p95 interval.

| Path | One-tab shell ready (ms) | Dense process CPU (ms) | Dense frame interval p95 (ms) |
| --- | ---: | ---: | ---: |
| DX12 | 333.2 | 2,718.8 | 4.56 |
| OpenGL | 631.4 | 4,921.9 | 59.05 |
| Automatic → DX12 | 425.8 | 2,906.3 | 4.42 |
| Experimental CPU | 215.0 | 468.8 | 1.68 |

DX12 used 45% less process CPU than OpenGL in the opaque dense workload. Its
median `frame.renderer` preparation scope was 0.040 ms, versus 0.061 ms for
OpenGL. The larger difference was in graphics submission and presentation:
OpenGL's median `gpu.present` scope was 27.77 ms, versus 0.09 ms for DX12.
DX12 instead waits mainly in image acquisition, about 3.42 ms. These are CPU-side
scope timings, not GPU timestamps or measurements of input-to-display latency.
DX12 uses Mailbox presentation where available; OpenGL uses FIFO here, so the
continuous-redraw comparison also reflects their different presentation policies.

| Path | Requested blur: dense process CPU (ms) | Frame interval p95 (ms) |
| --- | ---: | ---: |
| DX12 | 2,593.8 | 4.74 |
| OpenGL | 4,734.4 | 58.98 |
| Automatic → DX12 | 2,828.1 | 4.99 |

The experimental CPU renderer is a useful lower-cost reference. It currently
lacks image overlays, GPU filters, advanced underline styles, and corner radii,
so these text timings do not justify choosing it for the current design.

A final six-pair startup experiment alternated automatic selection and explicit
DX12, using the same executable and opaque one-tab configuration:

| Metric | Automatic | Explicit DX12 |
| --- | ---: | ---: |
| Median shell ready | 454.2 ms | 337.7 ms |
| Median first frame | 422.9 ms | 320.5 ms |
| Median graphics-instance initialization | 130.80 ms | 13.75 ms |
| Median complete graphics-context initialization | 179.75 ms | 60.61 ms |

Explicit DX12 reduced median shell readiness by 26%. Every pair favored it, by
91–136 ms. Most of the difference is in instance initialization rather than text
preparation. For this VM, launch Rio with `WGPU_BACKEND=dx12` in its process
environment when minimizing startup cost matters. The default already chooses
the same API; there is no reason to force Vulkan here.

All seven successful path/configuration combinations produced terminal glyphs
in 1440 × 900 window captures, including the tab bar. Captures used `PrintWindow`
after timing; foreground activation and desktop `BitBlt` capture were unavailable.
The DX12 image was also inspected directly. These checks validate rendered window
content, not RDP delivery cadence, foreground behavior, or desktop transparency.

[Machine-readable results](windows-vm-renderer-selection.json) record the medians,
paired startup savings, adapter identities, and capture checks. Raw profiles,
configs, drivers, images, and the executable remain in
`target/backend-selection-20261019/results.zip` on Linux and in
`E:\rio-backend-setup\results.zip` on Windows. The archive SHA-256 is
`45d260b8a64049b5e8aedc1e01a211ce9d268cdd73ac5b907a3eca8c9305e7e2`.

The executable SHA-256 is
`af968b36f5171283a3e3a270dd86dd2ff6e91ba0dc857568711bb78f1d5cd86b`.
All 45 timing captures completed their requested workloads. Benchmark processes
exited, six temporary scheduled tasks were removed, and the Windows checkout
was clean after restoring instrumentation. User Rio configuration, graphics
preferences, and power settings were unchanged.
