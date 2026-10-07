# TrackDoctor

Tells me why a lighthouse device glitched. When a Vive tracker or Index controller drifts, jumps or drops out, TrackDoctor lines up the pose stream, SteamVR's lighthouse log and the USB bus, then names a cause with the evidence attached: base station occlusion (and which base), the wireless link to the dongle dropping, a USB reset, IMU drift while the bases lost sight of it, a base going to sleep, or a jump that looks like a reflection.

After a session it ranks every tracker and controller by how long it spent not tracking, worst first.

## Install

Download `OpenVR-TrackDoctor-Setup-<version>.exe` from the [releases page](https://github.com/RealWhyKnot/OpenVR-TrackDoctor/releases) and run it. It installs for your Windows user only (no admin prompt) and registers with SteamVR as a startup app. From then on it starts by itself whenever SteamVR starts, records in the background without a window, and stops when SteamVR closes.

`trackdoctor autostart off` stops it starting with SteamVR, and `trackdoctor autostart on` turns that back on.

To uninstall, use Windows Settings, Apps, or the "Uninstall TrackDoctor" Start menu entry. It removes the SteamVR registration and offers to delete your recorded sessions too.

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

"Not tracking" adds up every outage plus every flap. It leaves out outages while the headset was in standby and moves of the whole playspace when the headset relocalizes. Neither is the device's fault. A flap is the tracking state dropping for under half a second while the pose is still valid. If one device flaps far more than your others, the bases can barely see it where it is.

## Live view

Run TrackDoctor from the Start menu to watch while you play. Tab, or the keys 1 to 4, switch between four views. The live view lists every device with its tracking state, battery, flaps and incidents, above a running list of what happened. In the summary view you get the ranked table above, updated as you go. The USB view shows which USB controller, hub and port each dongle is plugged into and which device uses it. If trackers on the same controller drop out together, suspect the USB side. On the room view, a top-down map shows the base stations and each device at its live position, numbered by trouble rank. Move a tracker to see which number it is.

Press n to name your devices (left foot, waist and so on). Notepad opens a text file that lists every serial, and the names show up as soon as you save. Press q to stop. TrackDoctor prints the summary and saves the full report.

If the background recorder is already running, the live view warns that the session is being recorded twice, and you'll end up with two folders for it.

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

`report` re-reads the saved events with the current rules, not the ones from when the session was recorded.

## What it saves

Everything stays on your PC, in `%LOCALAPPDATA%\trackdoctor`. Each session gets a folder under `sessions\` with `events.jsonl` (every signal), `verdicts.jsonl`, `devices.json`, `usb.json` and `report.txt`. The report is rewritten every 30 seconds while recording. If TrackDoctor crashes or you close the window, the last copy is still there. Your device names are in `names.txt`.

It reads SteamVR's `vrserver.txt` log and its per-device config files. It doesn't write to SteamVR's files, apart from registering itself as a startup app.

## Reading verdicts

Each verdict has a confidence: confident, likely or unsure. Confident means TrackDoctor saw a signal that only that cause produces, such as a USB unplug event or a SteamVR log line about that base station. Likely means the pattern fits but that signal wasn't there. Reflection verdicts are never better than likely, because a reflection, a brief occlusion and a solver glitch look the same from software.

## Build from source

Needs Rust (stable) on Windows. `cargo build --release` builds `trackdoctor.exe` and `trackdoctor-bg.exe` (the windowless background recorder). `installer\build.ps1` builds the installer and a portable zip into `target\installer\`. It needs [NSIS](https://nsis.sourceforge.io).

Releases are tagged `vYYYY.M.D.N`, with `-beta` for the nightly prereleases. Pushing a tag builds and publishes the installer and the zip.
