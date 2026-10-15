# Dev builds

Any build that doesn't come from a release tag is a dev build. Dev builds keep a debug log and record each dongle's radio traffic alongside the normal session. Release builds do neither, and the extra code never runs in them.

## Which builds are dev builds

`build.rs` decides once, at compile time, and sets `trackdoctor::DEV`:

- `TRACKDOCTOR_DEV=1` (or `true`, `yes`, `on`) forces a dev build. `TRACKDOCTOR_DEV=0` (or `false`, `no`, `off`) forces a release build.
- Without that, the version decides. `TRACKDOCTOR_VERSION` set to a tag version, `YYYY.M.D.N` or `YYYY.M.D.N-beta`, makes a release build. Anything else is a dev build: no version at all, `0.0.0.0-dev`, or a commit stamp like `2026.10.15.0-9030` from `version.txt`. The updater reads versions the same way, and a test keeps the two in step.

A dev build never offers updates, so an installed dev build stays put until you install a release over it.

The release workflow passes the tag version, so tagged installers and nightly betas are release builds. `cargo build`, and `installer\build.ps1` run by hand, give you a dev build. To check a release build locally:

```powershell
$env:TRACKDOCTOR_DEV = "0"; cargo build --release; Remove-Item Env:TRACKDOCTOR_DEV
```

The rules live in `src/channel.rs` and have unit tests.

## The debug log

Dev builds write `%LOCALAPPDATA%\trackdoctor\debug.log`. Each line has the local time, the thread name and the message:

```
2026-10-15 21:04:11.532 [packets] packets DDF8F5F1F6: listening (read-only)
```

When the log reaches 4 MB it becomes `debug.old.log` and a new one starts, so the two together stay under 8 MB. A panic writes its message and a full backtrace there before the normal panic handling runs.

What it records:

- startup, with the version and where the exe is
- connecting to SteamVR and SteamVR going away
- SteamVR starting a new run in `vrserver.txt`
- every USB plug and unplug the USB watcher sees
- each dongle the packet capture opens, skips or stops listening to, and why
- a one-line summary per dongle every minute
- a session starting, being dropped as empty, being saved (with its size) and being packed

To add a line, call `trackdoctor::dlog!` like `format!`. In a release build it compiles to nothing, and the arguments aren't evaluated. Nothing is written until `debug::init()` runs, which only the app does at startup, so tests and `tdinspect` leave the log alone. Background threads are started through `app::spawn`, which names them, so the name shows up in each line.

## Radio packet capture

While SteamVR is running, a dev build opens every Watchman dongle (USB `28DE:2101`) and reads its input reports. It opens them read-only and shares them with SteamVR the same way SteamVR shares them, so both get every report. It never sends anything to a dongle. When SteamVR closes, it closes the dongles and writes out what it has before the session is saved.

The dongle reports are the ones libsurvive documents: `0x23` carries one 29-byte packet, `0x24` carries two, and `0x26` means the tracker turned off. In each packet, byte 0 is the high byte of the tracker's 16-bit clock, byte 1 is the size, byte 2 is the clock's low byte and byte 3 holds the flags (`0x80` IMU, `0x40` meta, `0x20` input, `0x10` light, `0xE2` alone is a heartbeat). One clock step is 65536 ticks of a 48 MHz clock, about 1.37 ms, so it wraps every 89.5 s.

There is no signal strength in any of this. What the capture can measure is time:

- **Host gap**: 20 ms or more on the PC between two reports from the same dongle that carried packets. This also catches the PC being slow to read, including TrackDoctor itself.
- **Air gap**: the tracker's clock moved 4 or more steps (about 5.5 ms) between two packets that arrived back to back. The tracker stamps the packet, so this is time the radio really lost. Air time isn't counted across a host gap longer than 80 s, where the clock could have wrapped.
- **Odd**: the tracker's clock moved more than 50 ms further than the PC's clock did. That can't be a real gap, so it's counted on its own and left out of the clock counts. A tracker that restarted without the dongle reporting it off would look like this.

The 20 ms and 4-step thresholds are a first guess. The clock histogram in the summary shows what a normal session looks like, so tune them once there's real data.

Each session folder gets two files:

- `packets.jsonl`: one line per dongle per second (`second`), one line per gap of 20 ms or more on either clock (`gap`), and `note` lines when a dongle is opened or closed. It's gzipped with the rest when the session is packed.
- `packets.json`: totals per dongle, the device SteamVR paired with it, the clock-step histogram (0, 1, 2, 3, 4-7, 8-15, 16-63 and 64+ steps) and the 20 worst gaps. It's rewritten every 30 seconds and when the session ends.

Packet data doesn't count toward a session having something in it, so a session where nothing else happened is still dropped as empty.

The live feed gets one dim line the first time packets arrive, so you can tell the capture is running.

## tdinspect

`examples/tdinspect.rs` reads the data folder so you don't have to unpack anything by hand:

```powershell
cargo run --example tdinspect -- sessions
```

| Command | What it prints |
|---|---|
| `sessions` | every session, newest first, with size and which data it has |
| `packets [SESSION]` | the per-dongle table, clock histogram and worst gaps |
| `gaps [SESSION] [DONGLE]` | every packet gap as CSV |
| `events [SESSION] [TEXT]` | the event log, filtered to lines containing TEXT |
| `track [SESSION] [SERIAL]` | the movement track as CSV |
| `live-packets [SECONDS]` | listens to the dongles now and prints each second |
| `log [LINES]` | the end of `debug.log` |

SESSION is a folder path, a `session-...` folder name, or how many sessions back to go (0 is the newest, and the default). `live-packets` works without SteamVR, but a dongle only sends while its tracker is on.

## Adding a dev-only feature

1. Put the new code behind `if trackdoctor::DEV`, or `crate::DEV` inside the crate. Spawn any new background thread with `app::spawn` so the log shows its name.
2. Use `dlog!` for anything you'd want to see after a session.
3. If it saves data, write it through `SessionWriter` so it only lands in real sessions, and add the file names to `store::FILES` so deleting a session removes them. Add text files that grow to `store::PACKED_TEXT` so they get gzipped. Leave them out of `bytes_written` unless they should keep an otherwise empty session.
4. Add a `tdinspect` command that reads it back.
5. Run `cargo test`, then build once with `TRACKDOCTOR_DEV=0` and check that nothing new shows up.
