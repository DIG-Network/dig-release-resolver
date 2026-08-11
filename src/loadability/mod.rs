//! Will the host actually LOAD a downloaded artifact, once its bytes are verified? (dig_ecosystem#1870)
//!
//! Signature and digest verification prove an artifact is the intended bytes; they say NOTHING about
//! whether those bytes can start on THIS host. A `linux/x64` build linked against GTK sonames a
//! headless server lacks installs perfectly and then dies inside the dynamic linker before `main`; an
//! arm64 build dropped into the `linux/x64` slot dies at `execve` with `Exec format error` while every
//! soname it names resolves fine. Both look flawless to a digest.
//!
//! This module answers the loadability question WITHOUT running the artifact — it reads the ELF's own
//! dynamic-linking requirements out of its bytes ([`parse_elf_needs`]) and checks each against the host
//! ([`inspect_artifact`] / [`decide_loadability`]). It exists here, in a level-00 foundation crate, so
//! the INSTALL-time selector (`dig-installer`) and the UPDATE-time selector (`dig-updater`'s beacon)
//! reach the BYTE-IDENTICAL decision: a host must never oscillate between calling a build loadable and
//! calling it unloadable depending on which selector looked. A second implementation is banned — this
//! is the one.
//!
//! ## Never execute the candidate
//!
//! Loadability is answered by PARSING bytes, never by spawning the artifact. The component that most
//! needs the check (dig-app) parses no arguments and, run under a root beacon, would seal a master seed
//! and bind a signing socket — so executing a candidate to "see if it runs" is itself the harm. The
//! module reads files; it does not run them. The one subprocess it may spawn is the host's own
//! `ldconfig` at a trusted absolute path, purely to READ the linker cache.
//!
//! ## The decision is three-valued and deliberately asymmetric
//!
//! [`Loadability::Unloadable`] / [`Loadability::WrongMachine`] REFUSE; [`Loadability::Loadable`]
//! permits; and [`Loadability::Indeterminate`] — a non-ELF artifact, an unparseable image, a host whose
//! library set cannot be established, or any non-Linux host — permits too. A guard that cannot PROVE
//! harm must not act: refusing what it cannot establish would freeze every native-package and musl host
//! forever, including the security updates and the updater's own update. The check can only ever make a
//! selector do LESS, never more, and runs strictly after signature + digest verification.

mod elf;
mod loadable;

pub use elf::{
    parse_elf_needs, ElfNeeds, ElfParseError, EM_386, EM_AARCH64, EM_ARM, EM_RISCV, EM_X86_64,
};
pub use loadable::{
    decide_loadability, expand_runpath, host_checker, inspect_artifact, Host, Loadability,
    LoadabilityCheck, SonameResolver,
};

// --- helpers lifted with the ELF/loadable modules, kept crate-private so the facade stays curated ---

/// Replace every control character with the Unicode replacement char, leaving ordinary text intact.
///
/// The strings this sanitizes are sonames and paths read out of an attacker-supplied artifact, and
/// they are about to be written to a privileged process's journal and an operator's terminal — where a
/// bare carriage return or an ANSI CSI sequence can overwrite the very line that reports a refusal.
/// Each control byte is REPLACED (not stripped) so a forgery stays visible rather than erased.
fn without_control_chars(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

/// Whether `path` is an absolute path to an existing regular file, returned as-is when it is.
///
/// The absolute-path requirement is a security boundary: a bare program name would be resolved through
/// `$PATH`, and a selector running as root inherits a `$PATH` that begins with directories an
/// unprivileged user may write — so a planted `ldconfig` there would run as root. Only an absolute,
/// present file is trusted.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn trusted_absolute(path: std::path::PathBuf) -> Result<std::path::PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!("{} is not an absolute path", path.display()));
    }
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_file() => Ok(path),
        Ok(_) => Err(format!("{} is not a regular file", path.display())),
        Err(e) => Err(format!(
            "trusted program not found at {}: {e}",
            path.display()
        )),
    }
}

/// The first of `candidates` that passes [`trusted_absolute`], or an error listing them all.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn first_trusted(candidates: &[&str]) -> Result<std::path::PathBuf, String> {
    for candidate in candidates {
        if let Ok(path) = trusted_absolute(std::path::PathBuf::from(candidate)) {
            return Ok(path);
        }
    }
    Err(format!(
        "no trusted program found at any of: {}",
        candidates.join(", ")
    ))
}

/// How often [`wait_within`] polls the child while waiting for its budget to elapse.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(20);

/// Wait up to `budget` for `child` to exit: `Some(true)`/`Some(false)` for its success once it does,
/// `None` if the budget elapses first (the caller then kills it). Polling rather than a blocking
/// `wait()` is what makes the DEADLINE real — a child that never exits cannot stall the caller.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn wait_within(child: &mut std::process::Child, budget: std::time::Duration) -> Option<bool> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status.success()),
            // The status is unreadable; the child is finished either way, so stop waiting on it.
            Err(_) => return Some(false),
            Ok(None) => {}
        }
        if std::time::Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Kill a child that outlasted its budget and reap it, so a bounded subprocess leaves no orphan. Both
/// calls are best-effort — the child may have exited in the race, and failing to signal an
/// already-dead process is not an error worth reporting.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn kill_and_reap(child: &mut std::process::Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_text_survives_the_sanitizer_unchanged() {
        // The control on the sanitizer: mangling a real soname would be worse than not sanitizing,
        // because every refusal detail an operator reads passes through here.
        let plain = "libgtk-3.so.0, /usr/lib/x86_64-linux-gnu/libc.so.6";
        assert_eq!(without_control_chars(plain), plain);
    }

    #[test]
    fn a_forged_log_line_cannot_survive_the_sanitizer() {
        // CR returns the cursor to column 0, then a plausible success line is printed over the refusal
        // that was actually reported — the payload shape this defends against.
        let forged = "lib\r\x1b[2Kdig-updater: pass applied.so.0";
        let safe = without_control_chars(forged);
        assert!(
            !safe.contains('\r') && !safe.contains('\u{1b}'),
            "no carriage return or escape byte may reach the terminal: {safe:?}"
        );
        assert_eq!(
            safe.chars().count(),
            forged.chars().count(),
            "each control character is REPLACED, so the forgery stays visible rather than erased"
        );
    }

    #[test]
    fn trusted_absolute_rejects_a_relative_or_missing_program() {
        assert!(
            trusted_absolute(std::path::PathBuf::from("ldconfig")).is_err(),
            "a bare name would be resolved through $PATH and must be refused"
        );
        assert!(
            trusted_absolute(std::path::PathBuf::from("/definitely/not/here/ldconfig")).is_err(),
            "a missing absolute path is refused"
        );
    }

    #[test]
    fn trusted_absolute_accepts_an_existing_absolute_file_and_first_trusted_finds_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let present = dir.path().join("ldconfig");
        std::fs::write(&present, b"x").expect("write the stand-in program");
        let present = present.canonicalize().expect("an absolute path");
        assert_eq!(
            trusted_absolute(present.clone()).expect("an existing absolute file is trusted"),
            present
        );

        let present_str = present.to_string_lossy().into_owned();
        assert_eq!(
            first_trusted(&["/definitely/not/here/ldconfig", &present_str])
                .expect("the first present candidate is chosen"),
            present,
            "first_trusted skips the missing candidate and returns the present one"
        );
        assert!(
            first_trusted(&["/definitely/not/here/ldconfig"]).is_err(),
            "no present candidate is an error, never a bare-name fallback"
        );
    }
}
