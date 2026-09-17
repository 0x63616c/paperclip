# Architecture decision records

One file per decision that would otherwise have to be reconstructed from the
code. Each states what was decided, what it costs, and what would make it
wrong — that last part matters most here, because Stage 1 ran before the
device survey and several of these are bets.

| ADR | Decision |
|---|---|
| [0001](0001-workspace-layout-and-crate-naming.md) | Cargo workspace layout and crate naming |
| [0002](0002-cpu-rasterisation-and-desktop-backend.md) | CPU rasterisation, and where the desktop backend lives |
| [0003](0003-manifest-format-and-capability-grants.md) | `paper.toml` shape, and capabilities as install policy |
| [0004](0004-built-in-stroke-font-and-vector-pieces.md) | A built-in stroke font and vector chess pieces |
| [0005](0005-greyscale-first-palette.md) | A greyscale-first palette |
| [0006](0006-provisional-device-assumptions.md) | Recording device-facing assumptions instead of guessing |
| [0007](0007-display-transport-via-vendor-waveform-engine.md) | Presentation goes through the vendor waveform engine, not raw DRM |
| [0008](0008-runtime-only-units-and-no-root-filesystem-install.md) | Runtime-only units, and nothing on the root filesystem |
| [0009](0009-device-adapter-ffi-boundary.md) | The device adapter's FFI boundary |
