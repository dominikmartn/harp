# Linux package candidates

The supported candidate target is **Ubuntu 24.04, x86_64**. Other distributions,
architectures, and older glibc versions are not validated. The Linux packaging path builds
the full Harp GUI, including the input classifier; it does not provide a classifier-free
substitute when the model is missing.

## Current status: blocked on the model

The checkout currently contains a Git LFS pointer, not the required model bytes, at
`crates/input_classifier/models/onnx/bert_tiny_v3.onnx`. A source or packaging-fixture
check passing does **not** mean that a Linux release has been built or is ready to install.
Full release compilation, real classifier inference, and clean-runtime package acceptance
are **BLOCKED** until the exact model is available. No Linux release is published by these
workflows, and this document does not announce available packages.

The required model identity is fixed:

| Property | Value |
| --- | --- |
| Bytes | `17582121` |
| SHA-256 | `d649a9cf72bb8c188cea7f290290f12e3a3ac4031469cd5144f17a500b46b166` |

Fetch the repository's LFS objects using Git LFS, then validate before installing build
dependencies or starting Cargo:

```sh
git lfs pull --include='crates/input_classifier/models/onnx/bert_tiny_v3.onnx' --exclude=''
./script/linux/verify_release_model
```

A missing file, an LFS pointer, or different bytes must fail validation. Do not replace the
model with a stub, change its expected identity, or disable the classifier to bypass this
gate. If the public LFS remote cannot supply it, wait for the matching upstream model.

## Source checks versus release acceptance

`.github/workflows/linux-oss-checks.yml` runs on pull requests and pushes, without secrets
or a real model download. It checks shell and Python syntax without writing bytecode and
runs the model-validator, provenance, package-assembly, linuxdeploy, and runtime-orchestration
fixture tests. The runtime regression tests mock Docker; they do not start containers or
execute candidate code. These locally runnable checks validate mechanics and failure handling,
not inference, a real GUI, or clean-runtime acceptance. Passing them does not remove the model
blocker.

`.github/workflows/linux-oss-build.yml` is manual-only. It fetches LFS and runs the exact
model validator before dependency setup or compilation. It uses the public source tree,
read-only repository permissions, and bounded build parallelism. It neither uses Warp's
private setup actions nor installs shared agent skills.

Before public release, acceptance must cover all of the following on the same candidate:

- The exact model validator and a real classifier inference test.
- The full GUI release build and both package formats.
- Installation, launch, and removal checks in a clean Ubuntu 24.04 runtime environment.
- AppImage execution without the checkout, developer libraries, or build-tree resources.
- Desktop entry, icon, URL handler, launch aliases, package dependency, and update-policy
  checks, including the transition from the old `warp-terminal-oss` package.

Run GUI checks only under Xvfb with a private D-Bus session, disposable home directory,
and a clean environment. Never inject input into the main desktop session. A build host
with development libraries installed is not a clean-runtime acceptance environment.
`script/release_linux` must pass its isolated runtime gate before creating candidate
checksums and provenance. The manual workflow uploads only those tested candidates, not
a GitHub Release. Restoring the model alone does not authorize a public release.

## Reproducible build entry point

After model validation, install the public Linux build and packaging prerequisites.
Inspect setup scripts before running them: `script/linux/install_build_deps` currently
also calls `script/install_cargo_build_deps`, which attempts internal channel setup.
The OSS workflow instead installs its explicit public dependency list and uses the
toolchain pinned in `rust-toolchain.toml`; do not use the private release workflows or
`script/bootstrap` for this path.

Build from the repository root, using a new output directory outside the checkout:

```sh
CARGO_BUILD_JOBS=2 ./script/release_linux --output /path/to/harp-candidate
```

This entry point uses the locked Cargo dependencies and the `release` profile, building
the `warp-oss` GUI with `release_bundle`, `nld_classifier_v3`, and `nld_heuristic_v2`.
It prepares a `.deb`, an AppImage, `SHA256SUMS`, a build manifest, and a Git source archive
for the exact source revision. It is a preparation command, not a publication command.
Preserve those provenance files with any candidate under review. Verify the checksums
from inside the output directory with `sha256sum -c SHA256SUMS`.

The entry point reads the application version from `app/Cargo.toml` and defaults the
package version to that value followed by `-1` (Debian packaging revision 1). It does not
change the app's reported version. For a packaging-only rebuild, pass
`--version APP_VERSION-REVISION`, using the manifest's literal application version and a
positive integer revision. Change the app version separately when preparing a new
application release; the default command and workflow follow it automatically.

Package metadata defaults to the upstream Harp source/contact. A fork can explicitly set
`HARP_PACKAGE_MAINTAINER` and `HARP_SOURCE_URL` for its own maintainer and public source
location. Do not imply that a downstream build is maintained by Warp or upstream Harp.
For the manual GitHub workflow, set the repository variable `HARP_PACKAGE_MAINTAINER`
instead of embedding fork-specific contact details in the workflow source.

## Isolated runtime gate

`script/linux/test_release_runtime` checks the Debian package and AppImage sequentially in
disposable official Ubuntu 24.04 amd64 containers. It requires a local Linux Docker daemon
on a Unix socket, GNU `timeout`, and access to pull `ubuntu:24.04` and install runtime
packages. It resolves and records the image digest. Only preparation uses networking;
the runner disconnects and checks container networking before executing candidate code.
It never installs candidate packages on the host or uses the host desktop.
Containers run sequentially with defaults of two CPUs, 4 GiB memory, 256 processes,
and a 3 GiB `/work` tmpfs. `HARP_RUNTIME_CPUS`, `HARP_RUNTIME_MEMORY`,
`HARP_RUNTIME_PIDS`, and `HARP_RUNTIME_WORK_SIZE` override these bounds. The recorded
limits are part of each trial's evidence; writable container layers have no claimed
disk quota.

The declared desktop baseline is not a bare Ubuntu image or an any-Linux portability
guarantee. It explicitly installs `ca-certificates`, `xvfb`, `xauth`, `x11-utils`, `xdotool`,
`dbus-daemon`, `util-linux`, `python3`, `file`, `fontconfig`, `fonts-dejavu-core`, `libasound2t64`,
`libegl1`, `libgl1`, `libgl1-mesa-dri`, `mesa-vulkan-drivers`, `libvulkan1`, `libwayland-client0`,
`libwayland-egl1`, `libx11-6`, `libxcb1`, `libxcursor1`, `libxi6`, `libxkbcommon-x11-0`,
and `zlib1g`, plus their dependencies. The Debian trial also installs the package's declared
runtime dependencies. No build/development packages are installed. AppImage acceptance
is therefore limited to this desktop baseline, with Mesa software Vulkan rendering, not
evidence that all system libraries are bundled.

The AppImage requires the host's `libasound.so.2`, supplied on Ubuntu 24.04 by
`libasound2t64`, the same runtime dependency declared by the Debian package. ALSA is
intentionally excluded from bundling by the upstream
[AppImage excludelist](https://raw.githubusercontent.com/AppImageCommunity/pkg2appimage/master/excludelist)
to avoid conflicts with host audio plugins. Declaring this host dependency does not establish
working audio hardware or voice features; the runtime gate does not test either.

Each trial records the explicit baseline list in `baseline.txt` and installed package versions
from `dpkg-query` in `packages-before.txt` and `packages-runtime.txt`. The Debian trial also
records `dpkg-policy.txt` so runtime-image documentation exclusion rules remain visible
alongside the extracted package documentation checks. Keep these records, input hashes,
runtime logs, and `result.json`
with the candidate. The GUI trial uses a disposable unprivileged home, Xvfb, private D-Bus,
and a clean environment; it checks terminal input and ordinary exit as well as launch.

To rerun the gate on existing candidates:

```sh
./script/linux/test_release_runtime --deb /path/to/harp-terminal_APP_VERSION-REVISION_amd64.deb \
  --appimage /path/to/Harp-x86_64.AppImage --output-dir /path/to/empty-runtime-results
```

The optional output directory must already exist, be empty, be owned by the caller, and
not be a symlink; otherwise omit it for a newly created temporary directory. Standard
output contains the result directory on success; progress and errors go to standard error.
Exit codes are `0` for a passed trial, `1` for candidate failure, `2` for invalid input,
`3` for an unavailable environment, `124` for a deadline, `130` for interruption, and `143`
for termination. A conflicting host AppImage `binfmt_misc` handler is an environment
failure (`3`), not a failed candidate or an accepted native execution. Use a host without
that conflict for acceptance; the runner does not change host handlers.

`HARP_RUNTIME_MECHANICS_ONLY=1` explicitly labels fixture trials as mechanics-only.
`HARP_RUNTIME_BINFMT_COMPAT=1` is allowed only together with that setting: it uses a runtime
copy with three AppImage padding bytes changed, records both launcher hashes, and leaves
the original candidate unchanged. Such compatibility trials are **not** native candidate
acceptance, even when they pass. Release provenance rejects mechanics-only results and
compatibility launch modes. Do not use either setting to bypass the exact-model or native
runtime gates. Local mechanics checks and blocked real release acceptance are distinct
statuses; neither a mock test nor a compatibility trial establishes a releasable package.

## Package identity and updates

- Debian package: `harp-terminal`; primary launch command: `harp`.
- Compatibility launch command: `warp-terminal-oss`; internal executable: `warp-oss`.
- Desktop/application ID: `com.dominikmartn.Harp`.
- AppImage: `Harp-x86_64.AppImage`.
- User data and settings: `~/.harp`, separate from Warp.

The new Debian package conflicts with the old `warp-terminal-oss` package; do not assume
they can be installed side by side. It does not add Warp APT repositories or signing
keys. Linux updates are manual: install a newly reviewed Debian package or replace the
AppImage. The macOS GitHub/DMG updater is not a Linux update mechanism.

### Moving from the old package

Inspect the installed package and the transaction APT proposes before changing it.
The old package's removal script leaves its package configuration, Warp APT repository,
and signing key behind on ordinary removal. An explicit purge of that old package
removes its package configuration, repository, and key; this may affect an intentionally
configured Warp repository. Decide whether to retain them before purging rather than
blindly forcing a replacement or deleting repository files by hand.

If you choose to remove that old package configuration, run
`sudo apt purge warp-terminal-oss` and review APT's proposed changes. Preserve `~/.harp`
and any backups; purging package configuration is not a request to delete personal Harp
settings. Only install a new `harp-terminal` candidate after it has passed the acceptance
checks above. A release is still a separate, explicit maintainer decision; neither
workflow creates a GitHub Release, pushes tags, or publishes to an APT repository.
