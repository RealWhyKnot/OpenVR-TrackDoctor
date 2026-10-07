# TrackDoctor

Tells me why a lighthouse device glitched. When a Vive tracker or Index controller drifts, jumps or drops out, TrackDoctor lines up the pose stream, SteamVR's lighthouse log and the USB bus, then names a cause with the evidence attached: base station occlusion (and which base), the wireless link to the dongle dropping, a USB reset, IMU drift while the bases lost sight of it, a base going to sleep, or a jump that looks like a reflection.

After a session it ranks every tracker and controller by how much time it spent not tracking, so the worst one is at the top.

## Install

Download `OpenVR-TrackDoctor-Setup-<version>.exe` from the [releases page](https://github.com/RealWhyKnot/OpenVR-TrackDoctor/releases) and run it. It installs for your Windows user only (no admin prompt) and registers with SteamVR as a startup app. From then on it starts by itself whenever SteamVR starts, records in the background without a window, and stops when SteamVR closes.

`trackdoctor autostart off` stops it starting with SteamVR, and `trackdoctor autostart on` turns that back on.

To uninstall, use Windows Settings, Apps, or the "Uninstall TrackDoctor" Start menu entry. It removes the SteamVR registration and asks whether to delete your recorded sessions too.

## After a VR session

Open "TrackDoctor last session report" from the Start menu. It shows a ranked table like this one:

```
Session length 2h 13m. Worst first, ranked by time spent not tracking.
 #  device                          not tracking       incidents  flaps/h  main problem
 1  LHR-FD4FF7E2 tracker            ########    3m 14s        23    218.8  dropout, cause unclear
 2  LHR-10268F5C tracker            #####       1m 54s        15    388.7  dropout, cause unclear
 3  LHR-A66FAA1B tracker            #####       1m 39s        20    247.5  dropout, cause unclear
```

Under the table is a short list of things to try for each kind of problem, then the USB layout of your dongles.

"Not tracking" adds up every outage plus every flap. Outages while the headset was in standby, and moves of the whole playspace (the headset relocalizing), are left out because they are not the device's fault. A flap is the tracking state dropping for under half a second while the pose stays valid; a high flap rate on one device, compared with your others, usually means the bases barely see it where it sits.

## Live view

Run TrackDoctor from the Start menu to watch while you play. Tab, or the keys 1 to 4, switch between four views. The live view lists every device with its tracking state, battery, flaps and incidents, above a running list of what happened. The summary view is the ranked table above, updated as you go. The USB view shows which USB controller, hub and port each dongle is plugged into and which device uses it; trackers that drop together and share a controller point at the USB side. The room view is a top-down map with the base stations and each device at its live position, numbered by trouble rank, so moving a tracker shows which number it is.

Press n to name your devices (left foot, waist and so on). It opens a text file in Notepad with every serial listed; the names show up as soon as you save. Press q to stop: the summary is printed and the full report is saved.

If the background recorder is also running, the live view says so; both record the same session.

## Commands

```
trackdoctor                     live view
trackdoctor report [folder]     summary of the last recorded session, or of the given one
trackdoctor report --full       same, plus every incident with its evidence
trackdoctor report --log FILE   re-read a saved vrserver.txt into an older session
trackdoctor usb                 USB layout of your dongles
trackdoctor autostart on|off|status
trackdoctor dump                raw signal stream, for debugging
```

Add `--poses` to the live view or `dump` to also save `poses.csv` (about 10 rows per second per device).

`report` re-analyzes the saved events with the current rules, so older sessions benefit from later fixes.

## What it saves

Everything stays on your PC, in `%LOCALAPPDATA%\trackdoctor`. Each session gets a folder under `sessions\` with `events.jsonl` (every signal), `verdicts.jsonl`, `devices.json`, `usb.json` and `report.txt`. The report is rewritten every 30 seconds while recording, so it survives a crash or a closed window. Your device names live in `names.txt`.

It reads SteamVR's `vrserver.txt` log and its per-device config files and never writes to SteamVR's files, apart from registering itself as a startup app.

## Reading verdicts

Each verdict carries a confidence: confident, likely or unsure. Confident means an exclusive signal proved it, such as a USB unplug event or a log line naming the base. Likely means the pattern fits but the proving signal was absent. Reflection verdicts are never better than likely: from software, a reflection, a brief occlusion and a solver glitch look the same.

## Build from source

Needs Rust (stable) on Windows. `cargo build --release` builds `trackdoctor.exe` and `trackdoctor-bg.exe` (the windowless background recorder). `installer\build.ps1` builds the installer and a portable zip into `target\installer\`; it needs [NSIS](https://nsis.sourceforge.io) installed.

Releases are tagged `vYYYY.M.D.N`, with `-beta` for the nightly prereleases. Pushing a tag builds and publishes the installer and the zip.
