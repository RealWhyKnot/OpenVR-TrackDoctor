# TrackDoctor

Tells me why a lighthouse device glitched, not just that it did. When a Vive tracker or Index controller drifts, jumps, or drops out, it correlates the pose stream, SteamVR's lighthouse log, and the USB bus, then names a cause with the evidence attached: base station occlusion (and which base), RF dropout on the Watchman link, a USB reset, IMU dead-reckoning drift, a base going to standby, or a reflection-shaped jump.

## Run

```
cargo run --release            live dashboard, q quits
cargo run --release dump       raw signal stream to stdout
trackdoctor report <verdicts.jsonl>   re-render a past session
```

Every run writes `sessions/<id>/` next to the binary: `events.jsonl` (every signal), `verdicts.jsonl`, and `report.txt` on exit. Start it before or after SteamVR; it waits and never launches anything.

At startup it maps every Watchman dongle to its USB controller and warns when too many share one hub. That check alone found a real problem on my machine.

## Reading verdicts

Each verdict carries a confidence. High means an exclusive signal proved it (a USB unplug event, a log line naming the base). Medium means the pattern fits but the proof signal was absent. Reflection verdicts never exceed Medium: from software, a reflection, a brief occlusion, and a solver glitch look identical, so that call is honest guesswork with supporting evidence, not a diagnosis.
