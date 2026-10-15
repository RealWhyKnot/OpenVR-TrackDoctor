pub fn is_release(version: &str) -> bool {
    let text = version.trim();
    let text = text.split('+').next().unwrap_or(text);
    let text = text.strip_prefix(['v', 'V']).unwrap_or(text);
    let (numeric, suffix) = text.split_once('-').unwrap_or((text, ""));
    let parts: Vec<&str> = numeric.split('.').collect();
    (suffix.is_empty() || suffix.eq_ignore_ascii_case("beta"))
        && parts.len() == 4
        && parts.iter().all(|p| {
            !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().is_ok()
        })
}

pub fn is_dev(version: Option<&str>, forced: Option<&str>) -> bool {
    match forced.map(|f| f.trim().to_ascii_lowercase()).as_deref() {
        Some("1" | "true" | "yes" | "on") => true,
        Some("0" | "false" | "no" | "off") => false,
        _ => !version.is_some_and(is_release),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tagged_releases_and_betas_are_not_dev() {
        assert!(!is_dev(Some("2026.10.15.0"), None));
        assert!(!is_dev(Some("2026.10.15.3-beta"), None));
        assert!(!is_dev(Some("v2026.10.15.0"), None));
    }

    #[test]
    fn stamped_local_and_unversioned_builds_are_dev() {
        assert!(is_dev(Some("2026.10.15.0-9030"), None));
        assert!(is_dev(Some("0.0.0.0-dev"), None));
        assert!(is_dev(Some("2026.10.15"), None));
        assert!(is_dev(None, None));
    }

    #[test]
    fn the_override_wins_both_ways() {
        assert!(is_dev(Some("2026.10.15.0"), Some("1")));
        assert!(!is_dev(Some("2026.10.15.0-9030"), Some("0")));
        assert!(!is_dev(None, Some(" Off ")));
        assert!(is_dev(Some("2026.10.15.0-9030"), Some("maybe")));
    }
}
