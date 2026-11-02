//! Platform detection and environment workarounds.

/// True when running inside Windows Subsystem for Linux.
pub fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
}

/// Whether to hide the Wayland socket so the GUI falls back to X11.
///
/// On WSLg the Wayland connection of the GUI window drops randomly at
/// startup (`Connection reset by peer`, about 1 run in 6 measured), while
/// X11 was stable. Only done when an X11 display exists to fall back to.
pub fn should_prefer_x11(wsl: bool, has_x_display: bool) -> bool {
    wsl && has_x_display
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_x11_only_on_wsl_with_display() {
        assert!(should_prefer_x11(true, true));
        assert!(!should_prefer_x11(true, false)); // nothing to fall back to
        assert!(!should_prefer_x11(false, true)); // native Linux keeps Wayland
        assert!(!should_prefer_x11(false, false));
    }
}
