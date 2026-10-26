<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="images/hero-dark.png">
    <img src="images/hero-light.png" width="800" alt="Harp: the app icon and name above a Harp window with sessions grouped in the sidebar and a Claude Code session at work, with agents working, waiting for you, done and set aside">
  </picture>
</p>

<p align="center">
  Run many Claude Code and Codex agents side by side, in one calm sidebar that tells you which one needs you.
</p>

<p align="center">
  <a href="LICENSE-AGPL"><img alt="License: AGPL-3.0" src="https://img.shields.io/badge/license-AGPL--3.0-blue"></a>
  <img alt="Platform: macOS" src="https://img.shields.io/badge/platform-macOS-lightgrey">
</p>

Harp is an unofficial fork of the open-source [Warp](https://github.com/warpdotdev/warp) terminal.
It replaces the tab bar with a sidebar built for long-running coding agents and keeps the rest of
Warp as it is.

## What it adds

<table>
  <tr>
    <td width="50%" valign="top">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="images/sidebar-dark.png">
        <img src="images/sidebar.png" width="100%" alt="Sidebar rows showing needs you, background work, working, done and inactive">
      </picture>
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="images/working-rings-dark.gif">
        <img src="images/working-rings-light.gif" width="100%" alt="Working rings turning next to sessions whose agents are running">
      </picture>
    </td>
    <td width="50%" valign="top">
      <h3>Agent states at a glance</h3>
      <p>Each row shows whether its Claude Code or Codex session is working, waiting for you or
      done, with an unread mark until you look. A dot means background work is still running;
      a moon marks a tab you set aside. Cmd+R renames a tab.</p>
      <h3>Groups that stay put</h3>
      <p>Put tabs into named groups. With the tmux integration on, the group and the order are
      stored on the tmux sessions, so they survive a restart and other tmux clients see the
      same layout.</p>
    </td>
  </tr>
  <tr>
    <td width="50%" valign="top">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="images/limit-rings-dark.png">
        <img src="images/limit-rings.png" width="100%" alt="Usage-limit rings for Claude and Codex in the sidebar footer">
      </picture>
    </td>
    <td width="50%" valign="top">
      <h3>Limit rings</h3>
      <p>Two rings per provider in the footer: the 5 hour window and the weekly limit. Hover one
      for the numbers and reset times. Codex's come from its own session logs, Claude's from
      Claude Code's status line (see <a href="#limit-rings">below</a>). Harp fetches nothing and
      reads no credentials.</p>
      <h3>Phone-friendly tmux</h3>
      <p>Tabs attached to tmux follow their session: name, group, read and inactive flags.
      A session started elsewhere, for example from your phone over ssh, can ask for a tab
      with <code>tmux set-option @warp_adopt 1</code>.</p>
    </td>
  </tr>
</table>

Everything else is upstream Warp, including its AI and cloud features (agents, Warp Drive, session
sharing, login). They talk to Warp's servers exactly as upstream does; Harp adds no servers of its
own. Logging in through Harp is untested.

## Install

**Download:** grab the latest `Harp-<version>.dmg` from [Releases](../../releases/latest) and drag
Harp into Applications. Harp has its own bundle id (`com.dominikmartn.Harp`), URL scheme (`harp://`)
and settings folder (`~/.harp`), so it runs next to Warp without sharing anything.

**Updates:** Harp checks the latest GitHub release every 10 minutes, the same way Warp checks its
own server. When the release's version is newer than `version` in `app/Cargo.toml` of the running
build, Harp downloads its DMG in the background, checks that it is signed by Harp's Developer ID
team, and offers to restart into it. To try this on an installed build, launch it with an older
version: `open --env HARP_UPDATE_PRETEND_VERSION=0.0.1 /Applications/Harp.app`.

**Build from source** (macOS, Rust toolchain from `rust-toolchain.toml`):

```sh
./script/bootstrap   # one-time: toolchain and build dependencies
./script/run         # builds target/debug/bundle/osx/Harp.app and launches it
```

`script/macos/run` signs with the first "Apple Development" identity in your keychain and falls
back to an ad-hoc signature. A signed, notarizable release DMG comes from `script/release_macos`
(see `--help`).

### Linux (Ubuntu 24.04, x86_64)

Linux `.deb` and AppImage preparation is documented in [Linux package candidates](docs/linux-release.md).
**Full builds are currently blocked:** the checkout has only a Git LFS pointer for the required
classifier model. Passing source/fixture checks does not establish a working release; full
compilation, real inference, and clean-runtime package checks remain blocked. No Linux package
availability is announced here.

Once the exact model is available, the release entry point is:

```sh
git lfs pull --include='crates/input_classifier/models/onnx/bert_tiny_v3.onnx' --exclude=''
./script/linux/verify_release_model
CARGO_BUILD_JOBS=2 ./script/release_linux --output /path/to/harp-candidate
```

The package version defaults to the application version in `app/Cargo.toml` plus Debian revision
`-1`. The isolated runtime gate targets the documented Ubuntu desktop baseline, not every Linux
system. Mocked or compatibility-mode mechanics checks are not release acceptance.

The intended Debian package is `harp-terminal`, launched with `harp` (`warp-terminal-oss` remains
a compatibility command). The internal binary remains `warp-oss`; the application ID remains
`com.dominikmartn.Harp`. Linux updates are manual. See the release guide before replacing an old
`warp-terminal-oss` package: removal and purge have different effects on its Warp APT repository.

For source development, install the pinned Rust toolchain and public build dependencies, then
use `cargo build --locked -p warp --bin warp-oss --features gui -j 2`. This is not a release or
model-validation substitute. Normal startup still configures Claude Code's status line when
applicable, as described below. The sidebar uses your configured UI font; double-click a tab to
rename it (Cmd+R is macOS-only).

## Settings

### Limit rings

The rings are on by default and show the agents you use:

- **Codex:** read from the `rate_limits` Codex logs into `~/.codex/sessions` (or `$CODEX_HOME`).
  They appear once Codex has answered at least once.
- **Claude Code:** Claude Code hands its plan limits only to its status line. On first start, if
  `~/.claude/settings.json` has no `statusLine` yet, Harp adds one that saves the status line's
  input to `~/.harp/claude-statusline.json` and prints nothing. A status line you already have is
  left alone; to keep yours and get the rings, save the input first, for example
  `"command": "tee ~/.harp/claude-statusline.json | your-command"`. The rings appear after Claude
  Code's next answer, for Claude.ai plans (not API keys).

More providers can come from a JSON file another tool keeps current
([format](docs/limit-rings.md)); its entries replace Harp's own readings of the same name.

### Other extras

The other extras are off by default. Turn them on in `~/.harp/settings.toml`; the app reloads the
file when it changes.

```toml
[integrations.limit_rings]
enabled = true
file = "~/.cache/limit-rings.json"

[integrations.tmux]
enabled = true
```

| Setting | Default | Effect |
|---|---|---|
| `integrations.limit_rings.enabled` | `true` | Shows the limit rings in the sidebar footer. |
| `integrations.limit_rings.file` | `""` | Optional JSON file with more providers; `~` is your home folder. |
| `integrations.tmux.enabled` | `false` | Tmux sync (names, groups, order, flags, adopting sessions) and agent detection inside tmux. Polls every 10 s. |
| `integrations.tmux.session_script` | `""` | Optional script that renames and regroups instead of the built-in tmux commands: `<script> rename <shell_pid> <group> <title>`, `<script> group <session_id> <old_group> <new_group>`. |
| `appearance.vertical_tabs.full_tab_menu` | `true` | `false` shrinks the tab and group menus to the essentials. Also in Settings > Appearance > Tabs. |
| `agents.third_party.should_render_cli_agent_toolbar` | `false` | `true` brings back upstream's toolbar under CLI agent sessions. With it off, update the Warp plugin for Claude Code through Claude Code's plugin marketplace. |

A Claude Code hook that reports background tasks to the sidebar lives in
[integrations/claude-code](integrations/claude-code/README.md).

<details>
<summary>Details for contributors</summary>

**Removed on purpose:** the tab bar's search field, view options and tab-config buttons in the
sidebar layout (new tabs come from Cmd+T, a double-click or the right-click menu on empty sidebar
space), the sidebar divider option and the title-strip panel toggles.

**Development switches** (environment, at launch): `WARPOSS_STATUS_TRACE=1` logs every change of a
row's shown status to `~/Library/Logs/harp.log` as a `[status-check]` line, for regression checks
against an agent's transcript. `WARPOSS_SKIP_LOGIN=1` starts with a test user and fails every
authenticated request, for automated runs without a login. `WARP_DATA_PROFILE=<name>` (debug
builds) keeps settings and data in `~/.harp-<name>`.

**Identity:** the name comes from `app_display_name!()` in `app/src/lib.rs`. Bundle id, URL scheme
and settings folder are set in `app/Cargo.toml`, `app/src/bin/oss.rs`,
`crates/warp_core/src/channel/state.rs`, `crates/warp_core/src/paths.rs`, `script/macos/bundle`,
`script/macos/run`, `crates/warpui/src/browser.rs`, `app/channels/oss/com.dominikmartn.Harp.desktop`
and `flake.nix`. Internal names stay Warp's: the `warp-oss` binary, the crates, the `WARPOSS_*`
switches and the `@warp_*` tmux options other tmux clients read.

Run `./script/presubmit` (format, clippy, tests) before sending changes.

</details>

## Based on Warp

Harp is an unofficial fork of [warpdotdev/warp](https://github.com/warpdotdev/warp) and is not
made, endorsed or supported by Warp (Denver Technologies, Inc.) or affiliated with them. "Warp" is
their name. Please report issues with Harp here, not to Warp.

## Credits

- [Warp](https://github.com/warpdotdev/warp), the terminal Harp is built on, and the open-source
  projects it depends on ([list](https://docs.warp.dev/help/licenses)).
- Sidebar icons from the [Hugeicons](https://hugeicons.com) free set (MIT) and
  [Phosphor](https://phosphoricons.com) (MIT); the licenses are in
  `app/assets/bundled/svg/hugeicons/` and `app/assets/bundled/svg/phosphor/`.

## License

Like upstream, the code is under the [GNU AGPL v3](LICENSE-AGPL), except the `warpui` and
`warpui_core` crates, which are [MIT](LICENSE-MIT). If you run a modified version for others over a
network, the AGPL asks you to offer them its source.
