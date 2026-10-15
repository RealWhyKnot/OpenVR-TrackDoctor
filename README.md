# TrackDoctor

Tells me why a lighthouse device glitched. When a Vive tracker or Index controller drifts, jumps or drops out, TrackDoctor lines up the pose stream, SteamVR's lighthouse log and the USB bus, then names a cause with the evidence attached: base station occlusion (and which base), the wireless link to the dongle dropping, a USB reset, IMU drift while the bases lost sight of it, a base going to sleep, or a jump that looks like a reflection.

After a session it ranks every tracker and controller by how long it spent not tracking, worst first.

## Install

Download `OpenVR-TrackDoctor-Setup-<version>.exe` from the [releases page](https://github.com/RealWhyKnot/OpenVR-TrackDoctor/releases) and run it. It installs for your Windows user only (no admin prompt) and adds itself to SteamVR's startup apps. From then on SteamVR opens the TrackDoctor window when it starts. TrackDoctor records while that window is open and stops when you close it, so nothing runs that you can't see. To stop SteamVR opening it, untick "Open TrackDoctor when SteamVR starts" on the Settings page.

The portable zip has the same program. Open `trackdoctor.exe` once while SteamVR is running and it adds itself to SteamVR's startup apps the same way.

To uninstall, use Windows Settings, Apps. It takes TrackDoctor out of SteamVR's startup apps and offers to delete your recorded sessions too.

TrackDoctor checks the releases page when it opens. If there's a newer version it asks whether to update now, later, or skip that version, and Settings has a button to check on demand. The update goes in when you close the window, and TrackDoctor then opens again by itself. An installed copy runs the new setup; a copy from the zip swaps in the new exe.

## After a VR session

When SteamVR closes, the window moves from the Live page to the Summary page. It shows a ranked table like this one:

```
Session length 2h 13m. Worst first, ranked by time spent not tracking.
 #  device                          not tracking       incidents  flaps/h  main problem
 1  LHR-FD4FF7E2 tracker            ########    3m 14s        23    218.8  dropout, cause unclear
 2  LHR-10268F5C tracker            #####       1m 54s        15    388.7  dropout, cause unclear
 3  LHR-A66FAA1B tracker            #####       1m 39s        20    247.5  dropout, cause unclear
```

Older sessions are on the History page. Click one and every page shows that session instead of the live one until you press "Back to live". Replay opens it in the Room with a timeline under the view: press play (at 1×, 4×, 16× or 60×) or drag along the bar, and each device moves where it went, trailing the last few seconds behind it. Dropout spots appear as the replay reaches them, and the marks on the bar are the incidents. Clicking near one starts a few seconds before it, and the panel beside the view lists what happened around that moment.

Above the table, "What went wrong" lists the problems TrackDoctor can pin down from SteamVR's log, each with what to do about it:

- the base station map is out of date (SteamVR is still moving bases in it ten minutes in)
- a base station turned or shifted while SteamVR was running
- a tracker stops answering: its dongle gets radio data but can't reach the main board, SteamVR can't read its config, or its IMU data stops. All three come from packets getting lost between the dongle and the tracker, so the first thing to fix is where the dongle sits
- one device took over another one's dongle
- a base station restarted
- bursts of sync errors, where SteamVR couldn't match light from a base to the base it expected
- two base stations on the same channel
- a 1.0 base station whose rotors fell out of phase lock

The map check ignores SteamVR's first ten minutes, because it always nudges the bases a bit while it settles in.

SteamVR's own log is misleading about which base moved. When two bases disagree, it always moves whichever one sits farther from the origin base, so the base it names often isn't the one that shifted. To find the real one, I look at the distance and angle between each pair of bases, which SteamVR logs whenever a tracker gets a good look at both. When a base turns, at least two of its pairs change while the pairs without it stay put, and that's the base I name.

The Live page also puts each one in its feed the moment it shows up.

When the map is out of date, the Summary page shows a "Reset the base station map" button. It asks first, because it closes SteamVR and whatever game is running. It then renames `lighthousedb.json` to a dated backup and starts SteamVR again, which works out where the bases are in a minute or two. Do it once every base is where it'll stay. SteamVR treats the new map as a new room, so if your headset tracks with base stations you may have to run Room Setup again. To undo it, close SteamVR and rename the backup back to `lighthousedb.json`.

Under the table is a short list of things to try for each kind of problem.

The "Base stations" part at the bottom of the Summary page counts how often SteamVR moved each base in its map during the session, the biggest move, and how many of those moves it also applied to live tracking. Those are the pops where everything jumps at once. Bases that keep moving mean one of them is loose, sits on something that flexes, or was re-aimed while SteamVR still had its old position saved. It isn't always the one in the table, so look for the line that names the base that turned.

"Not tracking" adds up every outage plus every flap. It leaves out outages while the headset was in standby and moves of the whole playspace when the headset relocalizes. Neither is the device's fault. A flap is the tracking state dropping for under half a second while the pose is still valid. If one device flaps far more than your others, the bases can barely see it where it is.

## The window

The tabs along the top:

- Live lists every device with its tracking state, battery, flaps and incidents, above a running list of what happened. For an older session it can also show every event it saved, oldest first.
- Summary has the ranked table above, updated as you go.
- USB shows which USB controller, hub and port each dongle is plugged into and which device uses it. If trackers on the same controller drop out together, suspect the USB side.
- Room draws your play space in 3D: the base stations with the area each one can see, every device at its live position and facing, numbered by trouble rank, and a cross where each dropout started. Drag to turn it, right-drag to slide, scroll to zoom, or use the Top, Front, Side and 3D buttons. The floor is coloured by how many bases can see each spot at the height you pick (floor, waist or head), from red for none to green for three or more. Move a tracker to see which number it is. The view moves 6 times a second while the Room page is open. The other pages update once a second, so TrackDoctor stays light while you play.
- Bases shows how far SteamVR moved each base station, session by session.
- History lists every recorded session with its length, incidents and size on disk. Replay plays one back in the Room, Folder opens its folder, and Delete removes it after asking. "Keep sessions for" is set to ever until you change it; pick a year, 3 months or a month and older sessions are removed each time the list loads.
- Settings is where you name your devices (left foot, waist and so on), turn SteamVR startup on or off, and find the data folder.

## What it saves

Everything stays on your PC, in `%LOCALAPPDATA%\trackdoctor`. Each session gets a folder under `sessions\` with `events.jsonl` (every signal), `verdicts.jsonl`, `devices.json`, `usb.json`, `bases.json`, `room.json` (where the bases were), `report.txt` and `track.bin`, where every device moved. If you tick "Also save raw positions" in Settings, it adds `poses.csv` too, about 10 rows per second per device. The report is rewritten every 30 seconds while recording. If TrackDoctor crashes, the last copy is still there.

Once a session is over, TrackDoctor gzips the event, verdict, report and pose files and packs the movement into `track.gz`. It only keeps a new point when a device has moved 5 mm or turned 2° away from where the replay would put it anyway, or when its tracking state changes. Each file is read back and compared before the original is removed. A two-minute test session with four devices moving the whole time comes to about 16 KB. Sessions from older versions get gzipped the same way the next time the History list loads; they have no movement to replay. Your device names are in `names.txt` and your settings in `settings.json`.

It reads SteamVR's `vrserver.txt` log, its per-device config files and its saved base station map, `lighthousedb.json`. It doesn't write to SteamVR's files, apart from registering itself as a startup app and renaming the map when you ask it to reset it.

## Reading verdicts

Each verdict has a confidence: confident, likely or unsure. Confident means TrackDoctor saw a signal that only that cause produces, such as a USB unplug event or a SteamVR log line about that base station. Likely means the pattern fits but that signal wasn't there. Reflection verdicts are never better than likely, because a reflection, a brief occlusion and a solver glitch look the same from software.

## Build from source

Needs Rust (stable) on Windows. `cargo build --release` builds `trackdoctor.exe`, plus a small `trackdoctor-bg.exe` that only exists so older portable copies can update; all it does is open `trackdoctor.exe`. `installer\build.ps1` builds the installer and a portable zip into `target\installer\`. It needs [NSIS](https://nsis.sourceforge.io).

Releases are tagged `vYYYY.M.D.N`, with `-beta` for the nightly prereleases. Pushing a tag builds and publishes the installer and the zip.

A build that doesn't come from a release tag is a dev build. Dev builds also keep a debug log and record each dongle's radio traffic. [docs/dev-builds.md](docs/dev-builds.md) covers both, and the `tdinspect` example for reading session data.
