# Original startup animation for official DSH

An optional Cordis addon for the official `@deepseek-ai/dsh@0.2.0-rc.2`
Web profile. It adds the original DSH animation over the official web UI,
borrows `ctx.agentPresets.acquireScope()` to read the selected default preset's
`ctx.skills.snapshot({ cwd, scope })`, active host `ctx.loader.entries()` and
active default-preset `ctx.agentPresets.compositionInventory()` rows, and
does not patch installed DSH source or replace the `dsh` executable.

The package includes the original animation, fixed English voice assets and
the licensed Chinese font subset. Node.js `^22.19.0 || >=24.0.0` is required.
No Rust, Python (after packaging), browser extension, model key or TTS runtime
is needed to play the animation. Official DSH's own model configuration is
needed for conversations, as usual.

From the dsh-rust repository run `python scripts/install_native_startup.py`.
This builds a complete npm tarball and installs it globally together with the
pinned official CLI. Its independent command is `dsh-native`:

```sh
dsh-native web --startup
dsh-native startup profile --name CatShark
dsh-native startup enabled on
dsh-native startup next off
dsh-native web
```

Animation is off by default. `--startup` and `--no-startup` override this
launch; `startup next on|off` is claimed once when the first local official
web page opens after successful boot. Subsequent page refreshes in the same
tab/session do not replay a completed/skipped sequence. `--no-open` means
the choice remains pending until the page is opened. `--help`, config dumps,
and non-web profiles do not consume it. Preferences are isolated under
`$DSH_HOME/startup-animation` (default `~/.dsh/startup-animation`).

All other native arguments are forwarded, for example `dsh-native web --port
3081`. Native `web` and `--profile web` are supported. This adapter does not
invent a terminal UI: the pinned official release ships web, headless, ACP
and SDK profiles; terminal animation for this repository's Rust CLI remains
available through `dsh --startup`. Electron Desktop, remote/LAN hosting and
custom native profiles are not covered. The animation endpoints intentionally
work only through a local loopback URL.

Update with `git pull` followed by the same installer. Uninstall with
`npm uninstall --global @dsh-rust/native-startup`; the existing official/Rust
`dsh` and saved user data are untouched. Delete the isolated
`$DSH_HOME/startup-animation` folder separately only if preferences are no
longer needed. `python scripts/install_native_startup.py --pack-only` creates
a portable tarball under `target/native-startup-package`; install that file
on another machine using `npm install --global /path/to/the.tgz`.

Compatibility is pinned to the published official 0.2.0-rc.2 API, cross-checked
against upstream 5badb15009ae1756c3afe0ae0cef1faafc290ccc. The addon uses
`webServer.register`, `webserver/index-inject`, `connection.requestRejection`,
`skills.snapshot`, `agentPresets.acquireScope`,
`agentPresets.compositionInventory`, and `loader.entries`; an official version upgrade must re-run the native
integration test. No percentage, skill or plugin inventory is simulated.

The code in this directory is MIT-licensed. Font license and asset provenance
ship beside their assets. The fixed generated voice recordings and provenance
are the same as the main project's startup assets.
