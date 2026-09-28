//! Window-scoped desktop consent. This is an accidental-action guard, not an
//! application sandbox: an approved application can itself affect other apps.
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Supplied by the native driver, NEVER accepted as identity from model args.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowIdentity {
    pub pid: u32,
    /// Native process birth identity, so PID reuse cannot inherit consent.
    pub process_identity: String,
    pub window_id: u64,
    pub bundle_id: String,
    pub application: String,
    pub title: String,
}

/// Process ancestors protect the host carrying this conversation even when its
/// terminal has an unfamiliar bundle ID. Explicit additions can only deny more.
pub struct DesktopScope {
    protected_pids: HashSet<u32>,
    protected_bundles: HashSet<String>,
}

impl DesktopScope {
    /// Derive the control-channel ancestry from the OS, never from tool args.
    pub fn for_host(additional_bundles: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut ancestors = HashSet::new();
        let mut pid = std::process::id();
        while pid > 1 && ancestors.insert(pid) {
            // macOS denies full proc_pidinfo for root-owned `login`, but its
            // public parent PID remains available through ps. Do not stop the
            // ancestry walk there and silently leave the terminal unprotected.
            let parent = std::process::Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "ppid="])
                .env_clear()
                .env("LC_ALL", "C")
                .output()
                .map_err(|e| format!("cannot inspect desktop host ancestry: {e}"))?;
            if !parent.status.success() {
                return Err("cannot inspect desktop host ancestry".into());
            }
            pid = String::from_utf8_lossy(&parent.stdout)
                .trim()
                .parse()
                .map_err(|_| "invalid parent PID in desktop host ancestry")?;
        }
        Ok(Self::new(ancestors, additional_bundles))
    }

    pub fn new(
        ancestors: impl IntoIterator<Item = u32>,
        additional_bundles: impl IntoIterator<Item = String>,
    ) -> Self {
        let mut protected_pids: HashSet<u32> = ancestors.into_iter().collect();
        protected_pids.insert(std::process::id());
        // Additional protected applications are host policy supplied by the
        // assembly, not a vendor-specific list hidden in the desktop backend.
        let protected_bundles: HashSet<String> = additional_bundles.into_iter().collect();
        Self {
            protected_pids,
            protected_bundles,
        }
    }

    pub fn check(&self, window: &WindowIdentity) -> Result<(), String> {
        if window.pid == 0
            || window.process_identity.is_empty()
            || window.window_id == 0
            || window.bundle_id.trim().is_empty()
        {
            return Err("desktop target has no verified application/window identity".into());
        }
        if self.protected_pids.contains(&window.pid)
            || self.protected_bundles.contains(&window.bundle_id)
        {
            return Err("this application carries or can interrupt the agent's control channel; desktop control is refused".into());
        }
        Ok(())
    }

    /// Re-resolve immediately before executing. Selection of one window must
    /// not follow another process that later occupies the same native IDs.
    pub fn revalidate(
        &self,
        selected: &WindowIdentity,
        current: &WindowIdentity,
    ) -> Result<(), String> {
        self.check(selected)?;
        self.check(current)?;
        if selected.pid != current.pid
            || selected.process_identity != current.process_identity
            || selected.window_id != current.window_id
            || selected.bundle_id != current.bundle_id
        {
            return Err("the selected desktop window changed identity; list targets again".into());
        }
        Ok(())
    }
}

/// Native process birth time and parent PID. Microseconds distinguish PID reuse
/// without trusting an application name or a timestamp supplied by the model.
#[cfg(target_os = "macos")]
pub fn process_info(pid: u32) -> Result<(String, u32), String> {
    let pid = i32::try_from(pid).map_err(|_| "invalid desktop process ID")?;
    if pid <= 0 {
        return Err("invalid desktop process ID".into());
    }
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // The kernel writes a fixed-size POD struct, and it is only read on success.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read != size {
        return Err("cannot verify the desktop process identity; it may have exited".into());
    }
    let info = unsafe { info.assume_init() };
    Ok((
        format!("{}:{}", info.pbi_start_tvsec, info.pbi_start_tvusec),
        info.pbi_ppid,
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn process_info(_pid: u32) -> Result<(String, u32), String> {
    Err("window-scoped desktop control currently requires macOS".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn window() -> WindowIdentity {
        WindowIdentity {
            pid: 42,
            process_identity: "birth-1".into(),
            window_id: 7,
            bundle_id: "com.apple.TextEdit".into(),
            application: "TextEdit".into(),
            title: "Untitled".into(),
        }
    }
    #[cfg(target_os = "macos")]
    #[test]
    fn native_birth_identity_is_stable_and_the_real_host_ancestry_is_protected() {
        let first = process_info(std::process::id()).unwrap();
        assert_eq!(first, process_info(std::process::id()).unwrap());
        assert!(!first.0.is_empty());
        assert!(first.1 > 0);
        assert!(process_info(0).is_err());
        assert!(process_info(u32::MAX).is_err());
        let scope = DesktopScope::for_host([]).unwrap();
        for pid in [std::process::id(), first.1] {
            let mut target = window();
            target.pid = pid;
            assert!(
                scope.check(&target).is_err(),
                "the host and its parent cannot be desktop targets"
            );
        }
    }

    #[test]
    fn identities_come_from_the_driver_and_protected_apps_cannot_be_overridden() {
        let scope = DesktopScope::new(
            [99],
            ["com.apple.Terminal".into(), "example.network-proxy".into()],
        );
        assert!(scope.check(&window()).is_ok());
        let mut target = window();
        target.pid = 99;
        assert!(scope.check(&target).is_err());
        target = window();
        target.bundle_id = "com.apple.Terminal".into();
        assert!(scope.check(&target).is_err());
        target.bundle_id = "example.network-proxy".into();
        assert!(scope.check(&target).is_err());
        target.bundle_id.clear();
        assert!(scope.check(&target).is_err());
    }
    #[test]
    fn approval_does_not_follow_a_reassigned_window_or_application() {
        let scope = DesktopScope::new([], []);
        let approved = window();
        let mut current = approved.clone();
        current.pid += 1;
        assert!(scope.revalidate(&approved, &current).is_err());
        current = approved.clone();
        current.bundle_id = "different.application".into();
        assert!(scope.revalidate(&approved, &current).is_err());
        current = approved.clone();
        current.window_id += 1;
        assert!(scope.revalidate(&approved, &current).is_err());
        current = approved.clone();
        current.process_identity = "birth-2".into();
        assert!(scope.revalidate(&approved, &current).is_err());
        current = approved.clone();
        current.title = "Changed document title".into();
        assert!(scope.revalidate(&approved, &current).is_ok());
    }
}
