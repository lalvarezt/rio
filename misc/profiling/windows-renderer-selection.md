# Renderer selection on the Windows laptop

This comparison uses the NVIDIA RTX A500 Laptop GPU, driver 596.71, and Intel
Iris Xe graphics, driver 32.0.101.7088. Windows uses the WDDM driver model and
the Balanced power plan. The Rio window is opaque, with blur disabled.

NVIDIA Vulkan is selected for this machine. It combines near-identical startup
to DX12 with tighter frame pacing in these runs, and keeps the branch's Vulkan
path for translucent Windows surfaces. This is a choice for the measured laptop
and workload, rather than a change to Rio's default on other machines.

## Results

The initial five-run startup comparison gives 1655 ms for NVIDIA Vulkan,
1662 ms for NVIDIA DX12, 800 ms for Intel OpenGL, and 1747 ms for Intel Vulkan.
Selecting only NVIDIA's Vulkan driver gives 1677 ms, with no startup improvement.
Adapter identity was checked in separate diagnostic launches with logging enabled.
Disabling implicit Vulkan layers gives 1694 ms, also without an improvement.
Loader diagnostics identify NVIDIA's `VK_LAYER_NV_present` and
`VK_LAYER_NV_optimus`; neither driver nor layer filtering is retained.

The table uses eight-tab, 600-frame text redraws. CPU time includes startup and
the entire capture; it is not CPU time per frame. All times are milliseconds.

| Renderer and GPU | Frame interval | Frame interval p95 | Screen render scope | Process CPU |
| --- | ---: | ---: | ---: | ---: |
| Vulkan, NVIDIA | 16.680 | 19.212 | 1.301 | 656.25 |
| DX12, NVIDIA | 16.702 | 20.317 | 2.065 | 484.38 |
| OpenGL, NVIDIA | 16.653 | 22.659 | 6.458 | 2671.88 |
| OpenGL, Intel | 16.862 | 21.680 | 3.810 | 640.63 |
| Vulkan, Intel only | 16.649 | 19.159 | 1.117 | 500.00 |

NVIDIA Vulkan and DX12 deliver about 60 frames per second. Intel OpenGL averages
about 58.4 frames per second across each run's complete workload interval.
Intel Vulkan's rendering is fast, but its eight-tab startup is slower and more
variable: the median is 3789 ms, compared with 2819 ms for default Vulkan.
NVIDIA OpenGL starts in 1613 ms. It averages about 59.7 frames per second, but
has worse frame-interval tails and uses about four times Vulkan's process CPU
time. DX12 uses less CPU than Vulkan; the selection favors the measured frame
pacing and the Windows UI's transparency path.

The OpenGL comparison explicitly tests both GPUs. Without an OS preference,
OpenGL selects Intel despite Rio's high-performance adapter request. A temporary
Windows high-performance preference for `rio-profile.exe` selects NVIDIA. The
experiment restores the original preference afterward. The additional NVIDIA
OpenGL series uses five consecutive launches rather than alternating backend order.

## Remaining costs

In the one-tab NVIDIA Vulkan startup captures, GPU instance setup takes 769 ms,
device creation takes 46 ms, and surface configuration takes 232 ms. Adapter
selection takes about 12 ms. These nested scopes account for much of startup.

Eight-tab Vulkan captures spend a median of 1211 ms creating eight ConPTY
sessions, about 151 ms each. This is separate from PowerShell startup: the
profiling shell is cmd. One-tab startup spends about 87 ms loading fonts and
115 ms creating the native window. The warm tab strip takes about 0.072 ms per frame.

The next substantial startup improvement would come from reusing the running
Rio process and its initialized graphics state. Eight new tabs also pay for
eight independent console hosts; optimizing tab painting cannot remove that
cost. The current workload does not identify tab UI work as a frame-rate limit.

## Method

The profiling executable runs in the logged-in desktop session with cmd shells.
Startup captures use one tab and wait for a successfully submitted frame after
the shell's title marker. Drawing captures use eight tabs and submit 600 frames.
Before readiness, cmd fills the terminal with ASCII text. Each requested redraw
then rebuilds all visible terminal rows and draws the Windows tab strip.
This exercises row processing and presentation, without continuous PTY output.

Each sustained comparison has five repetitions. The DX12 and Vulkan variants
rotate order between repetitions. OpenGL and Intel-only Vulkan alternate order
in a separate batch. No builds run during capture. The comparison retains first
launches; no reboot or shader-cache purge is performed.

Reported frame timings are medians of per-run medians. Reported p95 values are
medians of per-run 95th percentiles. They measure CPU wall time and driver waits,
not GPU execution or input-to-display latency. Whole-process CPU time comes
from Windows `GetProcessTimes` and excludes child shells. The frame workload
uses normal display pacing, so it compares delivered frame cadence rather than
maximum uncapped GPU throughput.

The profiling executable is built from `0407f9e0e2` with the release settings
in the guide. Its SHA-256 is
`6e58ff08de866f8525e59c240d34976ac7f95015c40fe95e34432d8c62f8f61f`.

## Machine configuration

The Windows file `%LOCALAPPDATA%\rio\config.toml` selects Vulkan through the
existing platform override:

```toml
[platform]
windows.renderer.backend = "Vulkan"
```

The original configuration is backed up beside it as
`config.toml.before-renderer-profiling-2026-10-15`. No driver filters, clock locks,
global GPU preferences, or power-plan changes are retained. The temporary
high-performance preference for the profiling executable is restored to its
original absent value.

## Validation and capture locations

The new comparison and configuration checks contain 84 successful captures.
Each capture satisfies shell readiness and requested frame counts. Acquisition,
queue submission, presentation, and the frontend presentation scope have matching
call counts in every capture.

After applying the Windows configuration, three startup captures and three
600-frame text captures pass without a backend environment override. The selected
adapter log identifies NVIDIA Vulkan. The startup median is 1744 ms; text captures
have a 16.587 ms median frame interval and a 19.514 ms p95.

Both release executables compile on Windows. The rebuilt normal `rio.exe`
selects NVIDIA Vulkan, runs `cmd /d /c echo RIO_NORMAL_BUILD_OK`, and exits with
code zero. It creates no profiling report even with `RIO_PROFILE_OUTPUT` set.
The interactive profiling task is removed after validation.

Raw captures, summaries, and adapter logs are under
`D:\git-repos\rio\target\profiles\renderer-selection` and
`D:\git-repos\rio\target\profiles\renderer-selection-extra`.
Copies are in this checkout's `target/profiles` directory. The first folder
contains default Vulkan, DX12, NVIDIA-only Vulkan, and the resident-client
experiment. The second contains Intel Vulkan, both OpenGL adapters, the layer
experiment, and the configuration validation. The original one-run Intel OpenGL
probe is excluded from the five-run results table.

## GPU initialization and power settings

Keeping another Rio process open changes the result. Five fresh NVIDIA-only
Vulkan launches fall from a median of 1677 ms to 1147 ms, a 32% reduction.
Their instance setup falls from 765 ms to 162 ms. During this experiment,
`nvidia-smi` reports P0, graphics clocks of 1537 MHz, memory clocks of 6001 MHz,
and power draw of 10.39 W. The resident Rio remains open and idle during these
launches, then is terminated after measurement.

This demonstrates a benefit from GPU residency, but does not establish the
same saving from a power-plan change. Even with the resident client, fresh
processes still take about 1.15 seconds to submit the shell frame. Reusing
the existing process for additional windows also benefits from the branch's GPU-instance
cache and avoids loading fonts again.

NVIDIA documents that the Windows kernel driver stays loaded from Windows
startup until shutdown. Linux persistence mode addresses a different driver
lifecycle; this machine reports persistence mode as unavailable under WDDM.
See [NVIDIA's driver-persistence overview](https://docs.nvidia.com/deploy/driver-persistence/overview.html).

NVIDIA's **Prefer maximum performance** setting controls GPU performance while
applications run. It can reduce clock changes, but does not retain a terminated
Rio process's graphics instance, device, swapchain, or pipelines. The setting is
available in NVIDIA Control Panel under **Manage 3D settings**, **Program Settings**.
See [NVIDIA's power-management reference](https://www.nvidia.com/content/Control-Panel-Help/vLatest/en-us/mergedProjects/3D%20Settings/Manage_3D_Settings_%28reference%29.htm).
This comparison does not benchmark a changed NVIDIA power-management policy.

Windows also provides a per-application GPU preference. Rio's wgpu adapter
request already asks for high performance; a preference setting cannot skip
graphics API initialization. See [Microsoft's graphics-settings description](https://devblogs.microsoft.com/directx/navigating-the-redesigned-graphics-settings-page/).

The process-local `VK_LOADER_DRIVERS_SELECT` experiments select driver manifest
names discovered on this machine: `nv-vk64.json` and `igvk64.json`. Such a filter
can prevent Vulkan startup if a future installation has no matching driver.
See [Khronos's driver-filtering documentation](https://github.com/KhronosGroup/Vulkan-Loader/blob/main/docs/LoaderDriverInterface.md#driver-filtering).

## Reproduction

Build instructions and metric definitions are in [the profiling guide](README.md).
For each backend, substitute `dx12`, `vulkan`, or `gl`:

```cmd
python misc\profiling\run-windows.py target\release\rio-profile.exe --runs 5 --backend vulkan --output target\profiles\startup-vulkan
python misc\profiling\run-windows.py target\release\rio-profile.exe --runs 1 --frames 600 --tabs 8 --full-redraw --backend vulkan --output target\profiles\text-vulkan-1
```

Repeat the drawing command with a unique output directory for each repetition,
rotating backend order. To select a single Vulkan driver, add
`--env VK_LOADER_DRIVERS_SELECT=nv-vk64.json` or
`--env VK_LOADER_DRIVERS_SELECT=igvk64.json`.
