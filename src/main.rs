use trackdoctor::app;

const HELP: &str = "\
TrackDoctor finds out why your SteamVR trackers and controllers glitch.

  trackdoctor                     live view; q stops and shows which devices did worst
  trackdoctor report [folder]     summary of the last recorded session (or the given one)
  trackdoctor report --full       same, plus every incident with its evidence
  trackdoctor report --log FILE   re-read a saved vrserver.txt into an older session
  trackdoctor usb                 which USB controller and port each dongle is on
  trackdoctor autostart on|off    record automatically whenever SteamVR runs
  trackdoctor autostart status    show whether auto-start is on
  trackdoctor update              check for a newer release and install it
  trackdoctor dump                raw signal stream, for debugging
  add --poses to the live view or dump to also save poses.csv (~10 Hz per device)
";

fn main() -> anyhow::Result<()> {
    trackdoctor::update::cleanup_old();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let poses = args.iter().any(|a| a == "--poses");
    let full = args.iter().any(|a| a == "--full");
    args.retain(|a| a != "--poses" && a != "--full");
    let log = match args.iter().position(|a| a == "--log") {
        Some(i) if i + 1 < args.len() => {
            let path = args.remove(i + 1);
            args.remove(i);
            Some(path)
        }
        Some(_) => return Err(anyhow::anyhow!("--log needs a path to a vrserver.txt")),
        None => None,
    };
    match args.first().map(String::as_str) {
        None => app::live(poses),
        Some("report") => app::report(args.get(1).map(String::as_str), full, log.as_deref()),
        Some("usb") => app::usb_layout(),
        Some("dump") => app::dump(poses),
        Some("update") => trackdoctor::update::command(),
        Some("autostart") => {
            let code = app::autostart(args.get(1).map(String::as_str))?;
            std::process::exit(code)
        }
        Some("help" | "--help" | "-h" | "/?") => {
            println!("TrackDoctor {}\n", trackdoctor::VERSION);
            print!("{HELP}");
            Ok(())
        }
        Some("version" | "--version" | "-V") => {
            println!("TrackDoctor {}", trackdoctor::VERSION);
            Ok(())
        }
        Some(other) => Err(anyhow::anyhow!("unknown command '{other}'\n\n{HELP}")),
    }
}
