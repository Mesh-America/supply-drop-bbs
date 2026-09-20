//! Stable names for USB serial devices.
//!
//! `/dev/ttyACM0`, `/dev/ttyACM1`, `/dev/ttyUSB0` and so on are numbered in the
//! order the devices attach, so the same radio can be `ttyACM0` today and
//! `ttyACM1` after it is unplugged and plugged back in, or after a reboot. With
//! two radios that swap the BBS speaks each protocol to the wrong device. The
//! usual symptoms are handshake timeouts and framing errors.
//!
//! udev also keeps a symlink per device under `/dev/serial/by-id/`, named after
//! the USB vendor, product and serial number
//! (`usb-Heltec_HT-n5262_D42292EF51268EE1-if00`). Those names don't change when
//! the number does. This crate finds that alias for a device so the setup wizard
//! can write it and the transports can warn when a config still uses the
//! numbered name.
//!
//! An alias only identifies one device if the device reports a unique USB
//! serial number. A board with none, or two boards sharing one, get the same
//! name, and udev leaves which of them owns the link undefined. Callers that
//! have the USB serial numbers (the setup wizard does) must not trust an alias
//! for such a device.
//!
//! Linux only in practice: elsewhere the directory doesn't exist and every
//! lookup answers `None`.

use std::path::{Path, PathBuf};

/// Where udev keeps the stable per-device symlinks.
pub const BY_ID_DIR: &str = "/dev/serial/by-id";

/// What to tell an operator whose `serial_port` is a numbered name. Shared by
/// the transports so the wording stays the same.
pub const NUMBERED_PATH_ADVICE: &str = "serial_port is a numbered device name that changes when \
     radios are re-attached or attach in a different order, which can point the BBS at the wrong \
     radio. The stable alias is the radio currently on this port: if that is the radio you \
     want, set serial_port to it";

/// True for `/dev/ttyACM<n>` and `/dev/ttyUSB<n>`: the numbered names that
/// depend on attach order.
#[must_use]
pub fn is_numbered_usb_tty(path: &str) -> bool {
    ["/dev/ttyACM", "/dev/ttyUSB"].iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The link in `by_id_dir` that resolves to `device`, if any. Returns the link's
/// own path (`<by_id_dir>/<name>`), not the device it points at. If several
/// links resolve to the device the first by name wins, so the answer is stable.
#[must_use]
pub fn by_id_alias_in(by_id_dir: &Path, device: &Path) -> Option<PathBuf> {
    let target = std::fs::canonicalize(device).ok()?;
    let mut links: Vec<PathBuf> = std::fs::read_dir(by_id_dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    links.sort();
    links
        .into_iter()
        .find(|link| std::fs::canonicalize(link).is_ok_and(|resolved| resolved == target))
}

/// The `/dev/serial/by-id/...` alias of `device`, if udev has one.
#[must_use]
pub fn by_id_alias(device: &str) -> Option<String> {
    by_id_alias_in(Path::new(BY_ID_DIR), Path::new(device))
        .map(|link| link.to_string_lossy().into_owned())
}

/// If `configured` is a numbered `/dev/ttyACM<n>` / `/dev/ttyUSB<n>` name and
/// the device has an alias in `by_id_dir`, that alias. `None` for a path that
/// is already stable, isn't a numbered USB tty, or has no alias.
#[must_use]
pub fn stable_alias_suggestion_in(by_id_dir: &Path, configured: &str) -> Option<String> {
    suggestion(by_id_dir, configured, is_numbered_usb_tty(configured))
}

/// The lookup behind [`stable_alias_suggestion_in`], with the numbered-name
/// decision passed in so tests can exercise it against a temp directory (the
/// real check only accepts `/dev/ttyACMn` and `/dev/ttyUSBn`).
fn suggestion(by_id_dir: &Path, configured: &str, numbered: bool) -> Option<String> {
    if !numbered {
        return None;
    }
    by_id_alias_in(by_id_dir, Path::new(configured)).map(|link| link.to_string_lossy().into_owned())
}

/// [`stable_alias_suggestion_in`] against the real `/dev/serial/by-id`.
#[must_use]
pub fn stable_alias_suggestion(configured: &str) -> Option<String> {
    stable_alias_suggestion_in(Path::new(BY_ID_DIR), configured)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbered_usb_tty_names_are_recognised() {
        for yes in [
            "/dev/ttyACM0",
            "/dev/ttyACM12",
            "/dev/ttyUSB0",
            "/dev/ttyUSB3",
        ] {
            assert!(is_numbered_usb_tty(yes), "{yes}");
        }
        for no in [
            "",
            "/dev/ttyACM",
            "/dev/ttyUSB",
            "/dev/ttyACMx",
            "/dev/ttyACM0a",
            "/dev/ttyAMA0",
            "/dev/serial0",
            "/dev/ttyS0",
            "/dev/serial/by-id/usb-Heltec_HT-n5262_D42292EF51268EE1-if00",
            "COM3",
            "ttyACM0",
        ] {
            assert!(!is_numbered_usb_tty(no), "{no}");
        }
    }

    #[test]
    fn the_advice_names_the_risk_and_the_caveat() {
        assert!(NUMBERED_PATH_ADVICE.contains("wrong radio"));
        assert!(NUMBERED_PATH_ADVICE.contains("currently on this port"));
    }

    #[cfg(unix)]
    mod alias {
        use super::*;
        use std::os::unix::fs::symlink;

        struct Dev {
            root: tempfile::TempDir,
        }

        impl Dev {
            fn new() -> Self {
                let root = tempfile::tempdir().unwrap();
                std::fs::create_dir(root.path().join("dev")).unwrap();
                std::fs::create_dir(root.path().join("by-id")).unwrap();
                Self { root }
            }
            fn device(&self, name: &str) -> PathBuf {
                let p = self.root.path().join("dev").join(name);
                std::fs::write(&p, b"").unwrap();
                p
            }
            fn by_id(&self) -> PathBuf {
                self.root.path().join("by-id")
            }
            fn link(&self, name: &str, device: &str) {
                // Relative, as udev makes them (`../../ttyACM0`).
                symlink(
                    format!("../dev/{device}"),
                    self.root.path().join("by-id").join(name),
                )
                .unwrap();
            }
            /// The path of a device file in the temp tree, as a string.
            fn path_of(&self, name: &str) -> String {
                self.root
                    .path()
                    .join("dev")
                    .join(name)
                    .to_string_lossy()
                    .into_owned()
            }
        }

        #[test]
        fn finds_the_alias_of_a_device() {
            let d = Dev::new();
            let dev = d.device("ttyACM0");
            d.link("usb-Heltec_HT-n5262_AAAA-if00", "ttyACM0");
            assert_eq!(
                by_id_alias_in(&d.by_id(), &dev),
                Some(d.by_id().join("usb-Heltec_HT-n5262_AAAA-if00"))
            );
        }

        // The case that motivated this: two identical boards, told apart only
        // by serial number, must map to two different aliases.
        #[test]
        fn two_identical_boards_map_to_two_different_aliases() {
            let d = Dev::new();
            let acm0 = d.device("ttyACM0");
            let acm1 = d.device("ttyACM1");
            d.link("usb-Heltec_HT-n5262_D42292EF51268EE1-if00", "ttyACM0");
            d.link("usb-Heltec_HT-n5262_01E66D357489801B-if00", "ttyACM1");

            let a = by_id_alias_in(&d.by_id(), &acm0).unwrap();
            let b = by_id_alias_in(&d.by_id(), &acm1).unwrap();
            assert_ne!(a, b);
            assert_eq!(
                a,
                d.by_id().join("usb-Heltec_HT-n5262_D42292EF51268EE1-if00")
            );
            assert_eq!(
                b,
                d.by_id().join("usb-Heltec_HT-n5262_01E66D357489801B-if00")
            );
        }

        // After a re-attach the numbers swap but the alias follows the device:
        // the same alias resolves to a different device number.
        #[test]
        fn the_alias_follows_the_device_when_the_number_changes() {
            let d = Dev::new();
            let acm0 = d.device("ttyACM0");
            let acm1 = d.device("ttyACM1");
            let alias = "usb-Heltec_HT-n5262_D42292EF51268EE1-if00";

            d.link(alias, "ttyACM0");
            assert_eq!(
                by_id_alias_in(&d.by_id(), &acm0),
                Some(d.by_id().join(alias))
            );
            assert_eq!(by_id_alias_in(&d.by_id(), &acm1), None);

            // Unplug and re-attach: udev recreates the link, now on ttyACM1.
            std::fs::remove_file(d.by_id().join(alias)).unwrap();
            d.link(alias, "ttyACM1");
            assert_eq!(
                by_id_alias_in(&d.by_id(), &acm1),
                Some(d.by_id().join(alias))
            );
            assert_eq!(by_id_alias_in(&d.by_id(), &acm0), None);
        }

        #[test]
        fn no_alias_when_no_link_points_at_the_device() {
            let d = Dev::new();
            let dev = d.device("ttyACM0");
            d.device("ttyACM1");
            d.link("usb-Other_Board_ZZZZ-if00", "ttyACM1");
            assert_eq!(by_id_alias_in(&d.by_id(), &dev), None);
        }

        // Many links created in a scrambled order, so the result can only be
        // stable if the names are sorted.
        #[test]
        fn first_alias_by_name_wins_when_several_point_at_one_device() {
            let d = Dev::new();
            let dev = d.device("ttyACM0");
            for name in ["m", "z", "c", "q", "a-first", "x", "b", "k", "t", "e"] {
                d.link(&format!("usb-{name}-if00"), "ttyACM0");
            }
            assert_eq!(
                by_id_alias_in(&d.by_id(), &dev),
                Some(d.by_id().join("usb-a-first-if00"))
            );
        }

        // The device may itself be given as a link (a config that already holds
        // the alias): it must resolve to the same target, not be compared as a
        // plain string.
        #[test]
        fn a_device_given_as_a_link_resolves_to_its_target() {
            let d = Dev::new();
            d.device("ttyACM0");
            d.link("usb-Zeta-if00", "ttyACM0");
            d.link("usb-Alpha-if00", "ttyACM0");
            assert_eq!(
                by_id_alias_in(&d.by_id(), &d.by_id().join("usb-Zeta-if00")),
                Some(d.by_id().join("usb-Alpha-if00"))
            );
        }

        #[test]
        fn missing_directory_or_device_is_none_not_an_error() {
            let d = Dev::new();
            let dev = d.device("ttyACM0");
            assert_eq!(by_id_alias_in(&d.root.path().join("absent"), &dev), None);
            assert_eq!(
                by_id_alias_in(&d.by_id(), &d.root.path().join("dev/ttyACM9")),
                None
            );
        }

        #[test]
        fn a_dangling_link_is_ignored() {
            let d = Dev::new();
            let dev = d.device("ttyACM0");
            d.link("usb-Gone-if00", "ttyACM7"); // target doesn't exist
            d.link("usb-Real-if00", "ttyACM0");
            assert_eq!(
                by_id_alias_in(&d.by_id(), &dev),
                Some(d.by_id().join("usb-Real-if00"))
            );
        }

        // The suggestion is for numbered names only, and only when there is an
        // alias to suggest. The temp tree's paths aren't /dev/ttyACMn, so the
        // positive case drives `suggestion` directly with the shape check
        // already answered.
        #[test]
        fn a_numbered_device_with_an_alias_gets_the_alias_suggested() {
            let d = Dev::new();
            d.device("ttyACM0");
            d.link("usb-Real-if00", "ttyACM0");
            assert_eq!(
                suggestion(&d.by_id(), &d.path_of("ttyACM0"), true),
                Some(
                    d.by_id()
                        .join("usb-Real-if00")
                        .to_string_lossy()
                        .into_owned()
                )
            );
            assert_eq!(suggestion(&d.by_id(), &d.path_of("ttyACM0"), false), None);
        }

        #[test]
        fn suggestion_is_none_for_a_path_that_is_not_a_numbered_usb_tty() {
            let d = Dev::new();
            d.device("ttyACM0");
            d.link("usb-Real-if00", "ttyACM0");
            // Absolute temp path: not /dev/ttyACM<n>, so no suggestion.
            assert_eq!(
                stable_alias_suggestion_in(&d.by_id(), &d.path_of("ttyACM0")),
                None
            );
            // An alias path is already stable.
            assert_eq!(
                stable_alias_suggestion_in(
                    &d.by_id(),
                    d.by_id().join("usb-Real-if00").to_str().unwrap()
                ),
                None
            );
        }

        #[test]
        fn suggestion_is_none_when_the_numbered_device_does_not_exist() {
            let d = Dev::new();
            d.link("usb-Real-if00", "ttyACM0");
            // /dev/ttyACM<n> that this machine may or may not have: only the
            // shape matters, and a nonexistent number can't resolve.
            assert_eq!(
                stable_alias_suggestion_in(&d.by_id(), "/dev/ttyACM987654"),
                None
            );
        }
    }
}
