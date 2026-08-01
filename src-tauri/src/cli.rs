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
    /// Ask a RUNNING instance to exit cleanly (via single-instance IPC).
    /// Honored only if that instance armed dev-exit; a production kiosk
    /// refuses and logs. Never launches a station itself; exit 3 when no
    /// instance is running.
    pub quit: bool,
    /// Guided verification of the exit chord: synthetic delivery check, then
    /// a PHYSICAL chord press within 15s. Exit 0/1 (incident #2 2026-08-01).
    pub exit_probe: bool,
    pub config: Option<PathBuf>,
}

impl Cli {
    pub fn parse(args: impl Iterator<Item = String>) -> Self {
        let mut cli = Cli::default();
        // Debug builds arm the exit chord BY DEFAULT — `cargo run` must never
        // be a reboot trap again (incident 2026-08-01). Release stays opt-in;
        // production lockdown is unchanged. `--no-dev-exit` exists so debug
        // builds can still rehearse the locked posture deliberately.
        cli.dev_exit = cfg!(debug_assertions);
        let mut args = args.peekable();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--sim" => cli.sim = true,
                "--windowed" => cli.windowed = true,
                "--conformance" => cli.conformance = true,
                "--spike" => cli.spike = args.next().and_then(|n| n.parse().ok()).or(Some(20)),
                "--reader-probe" => cli.reader_probe = true,
                "--dev-exit" => cli.dev_exit = true,
                "--no-dev-exit" => cli.dev_exit = false,
                "--quit" => cli.quit = true,
                "--exit-probe" => cli.exit_probe = true,
                "--config" => cli.config = args.next().map(PathBuf::from),
                _ => {}
            }
        }
        cli
    }

    pub fn kiosk(&self) -> bool {
        !self.windowed
            && !self.conformance
            && self.spike.is_none()
            && !self.reader_probe
            && !self.quit
            && !self.exit_probe
    }

    /// Whether THIS launch registers the global exit chord. Verification and
    /// service modes never do (E7, incident #2): they exit by themselves and
    /// must not grab an OS-wide hotkey for their lifetime. --exit-probe arms
    /// through its own path.
    pub fn chord_armed(&self) -> bool {
        self.dev_exit
            && !self.conformance
            && self.spike.is_none()
            && !self.reader_probe
            && !self.quit
            && !self.exit_probe
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

    /// S2 (incident 2026-08-01): this build profile's default exit posture.
    /// Fails if the debug-default chord arming is ever removed.
    #[test]
    fn dev_exit_defaults_follow_build_profile() {
        let cli = Cli::parse(std::iter::empty());
        assert_eq!(
            cli.dev_exit,
            cfg!(debug_assertions),
            "debug builds must arm the exit chord by default; release must not"
        );
        assert!(Cli::parse(["--dev-exit".to_string()].into_iter()).dev_exit);
        assert!(!Cli::parse(["--no-dev-exit".to_string()].into_iter()).dev_exit);
    }

    /// E7 (incident #2, 2026-08-01): verification/service modes must never
    /// grab the global OS hotkey — a debug `--conformance` run used to own
    /// Ctrl+Alt+Shift+Q for its lifetime. Fails if that gate is removed.
    #[test]
    fn verification_and_service_modes_never_arm_the_chord() {
        let parse = |args: &[&str]| Cli::parse(args.iter().map(|s| s.to_string()));
        assert!(parse(&["--dev-exit"]).chord_armed());
        assert!(parse(&["--dev-exit", "--sim"]).chord_armed());
        assert!(!parse(&["--dev-exit", "--conformance"]).chord_armed());
        assert!(!parse(&["--dev-exit", "--spike", "5"]).chord_armed());
        assert!(!parse(&["--dev-exit", "--reader-probe"]).chord_armed());
        assert!(!parse(&["--dev-exit", "--quit"]).chord_armed());
        assert!(!parse(&["--dev-exit", "--exit-probe"]).chord_armed());
        assert!(!parse(&["--no-dev-exit"]).chord_armed());
    }

    /// E1/E3 (incident #2): service modes are not kiosk launches — they must
    /// not warn about lockdown or create kiosk windows.
    #[test]
    fn quit_and_probe_modes_are_not_kiosk() {
        let parse = |args: &[&str]| Cli::parse(args.iter().map(|s| s.to_string()));
        assert!(parse(&["--quit"]).quit && !parse(&["--quit"]).kiosk());
        assert!(parse(&["--exit-probe"]).exit_probe && !parse(&["--exit-probe"]).kiosk());
        assert!(!parse(&["--reader-probe"]).kiosk());
    }
}
