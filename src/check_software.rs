use crate::diagnose::{Finding, Rule};
use crate::event::DriverStatus;
use std::collections::BTreeMap;

const NO_HEADSET: [&str; 2] = ["HmdNotFound", "HmdDisplayNotFound"];

pub fn needs_card(status: &DriverStatus) -> bool {
    match status {
        DriverStatus::Failed(e) => !NO_HEADSET.iter().any(|n| e.contains(n)),
        _ => false,
    }
}

pub fn changed(name: &str, prev: Option<&DriverStatus>, now: &DriverStatus) -> bool {
    if prev == Some(now) {
        return false;
    }
    let DriverStatus::Failed(e) = now else {
        return false;
    };
    let card = needs_card(now);
    crate::dlog!(
        "software: driver {name} failed to load ({e}), {}",
        if card {
            "showing a card"
        } else {
            "no headset for it, staying quiet"
        }
    );
    card
}

pub fn any_failed(drivers: &BTreeMap<String, DriverStatus>) -> bool {
    drivers.values().any(needs_card)
}

fn reason(e: &str) -> &'static str {
    if e.contains("FileNotFound") {
        " because some of its files are missing"
    } else if e.contains("InterfaceNotFound") {
        " because it doesn't match this version of SteamVR"
    } else if e.contains("RuntimeOutOfDate") {
        " because it needs a newer version of SteamVR"
    } else if e.contains("HmdInUse") {
        " because another program is already using its headset"
    } else {
        ""
    }
}

pub fn findings(drivers: &BTreeMap<String, DriverStatus>) -> Vec<Finding> {
    drivers
        .iter()
        .filter(|(_, s)| needs_card(s))
        .map(|(name, status)| {
            let DriverStatus::Failed(e) = status else {
                unreachable!()
            };
            let why = reason(e);
            let (title, what, steps) = if name.eq_ignore_ascii_case("lighthouse") {
                (
                    "SteamVR couldn't load its base station tracking".to_string(),
                    format!(
                        "SteamVR's lighthouse add-on failed to start{why}. It tracks everything that uses base stations, so those devices can't be tracked until it loads."
                    ),
                    vec![
                        "Restart SteamVR.".to_string(),
                        "If it happens again, repair SteamVR: in Steam, right-click SteamVR, choose Properties, then Installed Files, then Verify integrity of game files.".to_string(),
                    ],
                )
            } else {
                (
                    format!("SteamVR couldn't load {name}"),
                    format!(
                        "SteamVR tried to start the {name} add-on and it failed{why}. It isn't running this session."
                    ),
                    vec![
                        "Restart SteamVR.".to_string(),
                        format!("Update or reinstall the program that installed {name}."),
                        "If you don't use it, turn it off in SteamVR's settings (Startup / Shutdown, Manage Add-ons).".to_string(),
                    ],
                )
            };
            Finding {
                rule: Rule::DriverFailed,
                key: name.clone(),
                title,
                what,
                steps,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnose::Diagnosis;
    use crate::signals::logtail::parse_line;

    const HEALTHY: [&str; 13] = [
        "Sun Oct 25 2026 15:03:47.070 [Info] - vrserver 2.18.4 startup with PID=35140, config=C:\\Program Files (x86)\\Steam\\config, runtime=C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR, arch=win64",
        "Sun Oct 25 2026 15:03:47.535 [Info] - Loaded server driver VirtualDesktop (IServerTrackedDeviceProvider_004) from C:\\Program Files\\Virtual Desktop Streamer\\OpenVRDriver\\bin\\win64\\driver_VirtualDesktop.dll",
        "Sun Oct 25 2026 15:03:47.535 [Warning] - Not loading driver prism because it is disabled in settings",
        "Sun Oct 25 2026 15:03:47.556 [Info] - Loaded server driver 01spacecalibrator (IServerTrackedDeviceProvider_004) from D:\\Github\\OpenVR\\OpenVR-SpaceCalibrator\\build\\01spacecalibrator\\bin\\win64\\driver_01spacecalibrator.dll",
        "Sun Oct 25 2026 15:03:47.586 [Info] - Loaded server driver vrlink (IServerTrackedDeviceProvider_004) from C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR\\drivers\\vrlink\\bin\\win64\\driver_vrlink.dll",
        "Sun Oct 25 2026 15:03:47.586 [Warning] - Not loading driver 01wkopenvr because it is disabled in settings",
        "Sun Oct 25 2026 15:03:47.586 [Warning] - Not loading driver GapTrack because it is disabled in settings",
        "Sun Oct 25 2026 15:03:47.586 [Warning] - Not loading driver SmoothTracking because it is disabled in settings",
        "Sun Oct 25 2026 15:03:47.884 [Info] - Loaded server driver lighthouse (IServerTrackedDeviceProvider_004) from C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR\\drivers\\lighthouse\\bin\\win64\\driver_lighthouse.dll",
        "Sun Oct 25 2026 15:03:48.151 [Info] - Loaded server driver oculus (IServerTrackedDeviceProvider_004) from C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR\\drivers\\oculus\\bin\\win64\\driver_oculus.dll",
        "Sun Oct 25 2026 15:03:48.399 [Info] - Loaded server driver oculus_legacy (IServerTrackedDeviceProvider_004) from C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR\\drivers\\oculus_legacy\\bin\\win64\\driver_oculus_legacy.dll",
        "Sun Oct 25 2026 15:03:48.399 [Warning] - Not loading driver gamepad because it is disabled in settings",
        "Sun Oct 25 2026 15:03:48.399 [Warning] - Not loading driver null because it is disabled in settings",
    ];

    fn label(s: &str) -> String {
        s.to_string()
    }

    fn replay(lines: &[&str]) -> (Diagnosis, Vec<bool>) {
        let mut d = Diagnosis::default();
        let mut signalled = Vec::new();
        for l in lines {
            let ev = parse_line(l, 0).unwrap_or_else(|| panic!("didn't parse: {l}"));
            signalled.push(d.ingest(&ev));
        }
        (d, signalled)
    }

    fn cards(d: &Diagnosis) -> Vec<Finding> {
        d.findings(&label)
            .into_iter()
            .filter(|f| f.rule == Rule::DriverFailed)
            .collect()
    }

    #[test]
    fn a_normal_startup_shows_no_driver_card() {
        let (d, signalled) = replay(&HEALTHY);
        assert_eq!(d.drivers.len(), 12);
        assert!(cards(&d).is_empty());
        assert!(!signalled.iter().any(|s| *s));
    }

    #[test]
    fn a_driver_with_no_headset_attached_stays_quiet() {
        let mut lines = HEALTHY.to_vec();
        lines[1] = "Sun Oct 25 2026 15:03:47.535 [Error] - Unable to load driver VirtualDesktop because of error VRInitError_Init_HmdNotFound(108). Skipping.";
        lines.push("Sun Oct 25 2026 15:03:48.400 [Error] - Unable to load driver oculus because of error VRInitError_Init_HmdNotFoundPresenceFailed(126). Skipping.");
        let (d, signalled) = replay(&lines);
        assert!(matches!(
            d.drivers["VirtualDesktop"],
            DriverStatus::Failed(_)
        ));
        assert!(matches!(d.drivers["oculus"], DriverStatus::Failed(_)));
        assert!(cards(&d).is_empty());
        assert!(!signalled.iter().any(|s| *s));
    }

    #[test]
    fn an_add_on_that_fails_to_load_gets_a_card_once() {
        let mut lines = HEALTHY.to_vec();
        lines[3] = "Sun Oct 25 2026 15:03:47.556 [Error] - Unable to load driver 01spacecalibrator because of error VRInitError_Init_InterfaceNotFound(105). Skipping.";
        let (mut d, signalled) = replay(&lines);
        assert_eq!(
            signalled.iter().filter(|s| **s).count(),
            1,
            "only the failed line signals"
        );
        assert!(signalled[3]);
        let found = cards(&d);
        assert_eq!(found.len(), 1);
        let f = &found[0];
        assert_eq!(f.key, "01spacecalibrator");
        assert_eq!(f.title, "SteamVR couldn't load 01spacecalibrator");
        assert_eq!(
            f.what,
            "SteamVR tried to start the 01spacecalibrator add-on and it failed because it doesn't match this version of SteamVR. It isn't running this session."
        );
        assert_eq!(f.steps.len(), 3);
        assert!(f.steps[1].contains("01spacecalibrator"));
        assert_eq!(d.fresh(found.clone()).len(), 1);
        assert!(d.fresh(found).is_empty());
    }

    #[test]
    fn the_card_goes_away_when_steamvr_restarts_and_loads_it() {
        let mut lines = HEALTHY.to_vec();
        lines[3] = "Sun Oct 25 2026 15:03:47.556 [Error] - Unable to load driver 01spacecalibrator because of error VRInitError_Init_InterfaceNotFound(105). Skipping.";
        lines.extend_from_slice(&HEALTHY);
        let (d, _) = replay(&lines);
        assert!(cards(&d).is_empty());
    }

    #[test]
    fn a_failed_lighthouse_driver_points_at_repairing_steamvr() {
        let mut lines = HEALTHY.to_vec();
        lines[8] = "Sun Oct 25 2026 15:03:47.884 [Error] - Unable to load driver lighthouse because of error VRInitError_Init_FileNotFound(103). Skipping.";
        let (d, _) = replay(&lines);
        let found = cards(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].title,
            "SteamVR couldn't load its base station tracking"
        );
        assert!(found[0].what.starts_with(
            "SteamVR's lighthouse add-on failed to start because some of its files are missing."
        ));
        assert!(found[0].steps[1].contains("Verify integrity of game files"));
    }

    #[test]
    fn an_unknown_error_still_gets_a_card_without_a_guessed_reason() {
        let mut lines = HEALTHY.to_vec();
        lines.push("Sun Oct 25 2026 15:03:48.500 [Error] - Unable to load driver mytracker because of error VRInitError_Driver_Failed(200). Skipping.");
        let (d, _) = replay(&lines);
        let found = cards(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(
            found[0].what,
            "SteamVR tried to start the mytracker add-on and it failed. It isn't running this session."
        );
    }
}
