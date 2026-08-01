//! Launch modes. No CLI framework — four flags don't warrant one.

use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct Cli {
    /// Sim console window + sim_* commands enabled.
    pub sim: bool,
    /// Development posture: 1280×720 window instead of kiosk fullscreen.
    pub windowed: bool,
    /// Load the conformance harness instead of the UI; exit 0/1/2 with results.
    pub conformance: bool,
    /// Reload-freshness spike: N cycles of state-mutate → document reload,
    /// asserting every fresh document boot-pulled CURRENT truth (zero stale
    /// bakes). Exit 0/1.
    pub spike: Option<u32>,
    /// Guided hardware verification of the detection path against the real
    /// reader (no windows, no webview). Exit 0/1.
    pub reader_probe: bool,
    /// Dev escape hatch: enables the Ctrl+Alt+Shift+Q exit chord in kiosk
    /// mode. Without this flag the chord's listener is never even injected
    /// and the command refuses — production lockdown is unchanged.
    pub dev_exit: bool,
    pub config: Option<PathBuf>,
}

impl Cli {
    pub fn parse(args: impl Iterator<Item = String>) -> Self {
        let mut cli = Cli::default();
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--sim" => cli.sim = true,
                "--windowed" => cli.windowed = true,
                "--conformance" => cli.conformance = true,
                "--spike" => cli.spike = args.next().and_then(|n| n.parse().ok()).or(Some(20)),
                "--reader-probe" => cli.reader_probe = true,
                "--dev-exit" => cli.dev_exit = true,
                "--config" => cli.config = args.next().map(PathBuf::from),
                _ => {}
            }
        }
        cli
    }

    pub fn kiosk(&self) -> bool {
        !self.windowed && !self.conformance && self.spike.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flags() {
        let cli = Cli::parse(
            ["--sim", "--windowed", "--config", "c.toml"].iter().map(|s| s.to_string()),
        );
        assert!(cli.sim && cli.windowed && !cli.conformance);
        assert_eq!(cli.config.as_deref(), Some(std::path::Path::new("c.toml")));
        assert!(!cli.kiosk());
    }

    #[test]
    fn default_is_kiosk() {
        assert!(Cli::parse(std::iter::empty()).kiosk());
    }
}
