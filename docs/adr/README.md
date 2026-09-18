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
| [0010](0010-input-coordinate-transforms.md) | Input coordinate transforms are a hardware constant |
| [0011](0011-takeover-ordering-and-the-restore-guarantee.md) | Takeover ordering, and what guarantees the restore |
| [0012](0012-supervisor-shape-and-where-it-is-proven.md) | The supervisor's shape, and where it is proven |
| [0013](0013-package-archive-and-the-signed-release-envelope.md) | The `.paperpkg` archive, and exactly which bytes are signed |
| [0014](0014-install-transaction-durability-and-rollback.md) | The install transaction: durability, activation and rollback |
| [0015](0015-settings-default-app-and-host-boundary.md) | Settings is a default app, and its Host boundary |
| [0016](0016-app-protocol-and-the-sdk-boundary.md) | The app protocol, and what the SDK is not |
| [0017](0017-chess-rules-library.md) | Chess rules library, and where it lives |
| [0018](0018-launch-request.md) | A fourth request: `Launch` |
| [0019](0019-platform-update-transaction.md) | The platform update transaction: stage, activate, verify, commit, roll back |
| [0020](0020-mac-side-device-transport.md) | The Mac-side device transport, and where auto-discovery is still unproven |
| [0021](0021-per-cell-damage-and-the-sudoku-app.md) | How an app claims per-cell damage, and Sudoku as the worked example |
| [0022](0022-app-entrypoint-binaries-and-stdio-transport.md) | App entrypoint binaries, and stdio as the launch transport |
| [0023](0023-paperctl-logs-doctor-deploy.md) | `paperctl logs`, `doctor` and `deploy`, replacing a set of personal shell aliases |
| [0024](0024-pinned-image-and-backup-scope.md) | The pinned OS image is load-bearing, and what the backup covers |
| [0025](0025-the-effects-spine-and-real-observability.md) | The effects spine and real observability |
| [0026](0026-declared-release-manifest-and-the-publish-planner.md) | Declared release manifest, and the publish planner |
| [0027](0027-paperclip-is-the-resident-stock-is-a-grantable-app.md) | Paperclip is the resident; stock is one of the things it may grant the display to |
| [0028](0028-system-services-over-the-protocol.md) | System services over the protocol, and what stays out of this pass |
| [0029](0029-https-catalog-transport-and-cross-compiling-ring-for-the-device.md) | HTTPS catalog transport, and cross-compiling ring for the device |
| [0030](0030-signing-off-the-build-machine.md) | Signing off the build machine |
| [0031](0031-layout-as-a-value-and-one-damage-accumulator.md) | Layout as a value, and one damage accumulator |
| [0032](0032-refreshing-the-recovery-bootstrap.md) | Refreshing the recovery bootstrap: when it runs, how staleness is surfaced, and closing a no-live-binary window |
| [0033](0033-compositor-core-buffer-lifecycle.md) | The compositor core: pool descriptor, buffer lifecycle, and what stays out of this stage |
| [0034](0034-gesture-arbitration-the-escape-pinch-edge-swipes-and-app-vs-system.md) | Gesture arbitration: the escape pinch, edge swipes, and app-vs-system |
| [0035](0035-system-chrome-a-drawing-order-guarantee-not-a-permission-check.md) | System chrome: a drawing-order guarantee, not a permission check |
| [0036](0036-why-takeover-not-in-process.md) | Why takeover, not in-process |
| [0037](0037-compositor-accept-loop-and-crash-hang-isolation.md) | The compositor accept loop, its client wire, and crash/hang isolation |
| [0038](0038-sleep-and-lock-as-a-compositor-overlay.md) | Sleep and lock as a compositor overlay, not a session hack |
| [0039](0039-everything-is-an-app-the-compositor-as-a-service.md) | Everything is an app: the compositor as a long-running service |
| [0040](0040-staging-the-compositor-into-the-platform-release.md) | Staging the compositor into the platform release |
| [0041](0041-signing-in-ci.md) | Signing in CI |
