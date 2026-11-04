//! A process's descendants: whether any of them runs outside the
//! foreground process group. Only pids and pgids are read.

/// Whether a process under `root` runs outside process group `foreground`.
///
/// That is a job the shell put in the background, or a command a program
/// detached into its own group. A failed query is "no".
#[must_use]
pub fn background_descendant(root: i32, foreground: i32) -> bool {
    root > 0
        && outside_group(root, foreground, child_pids, |pid| {
            nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid)))
                .ok()
                .map(nix::unistd::Pid::as_raw)
        })
}

/// The pure walk behind [`background_descendant`], bounded so a fork bomb
/// can't stall the detector tick.
fn outside_group(
    root: i32,
    foreground: i32,
    children: impl Fn(i32) -> Vec<i32>,
    pgid: impl Fn(i32) -> Option<i32>,
) -> bool {
    const MAX_VISITED: usize = 512;
    let mut stack = children(root);
    let mut visited = 0;
    while let Some(pid) = stack.pop() {
        visited += 1;
        if visited > MAX_VISITED {
            return false;
        }
        if pgid(pid).is_some_and(|group| group != foreground) {
            return true;
        }
        stack.extend(children(pid));
    }
    false
}

#[cfg(target_os = "linux")]
/// Every thread's `children` list: a child forked off a worker thread
/// is listed under that thread, not the leader.
fn child_pids(pid: i32) -> Vec<i32> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .flatten()
        .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
        .flat_map(|list| {
            list.split_whitespace()
                .filter_map(|pid| pid.parse().ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(target_os = "macos")]
/// `proc_listchildpids`, sized by a first call that counts.
fn child_pids(pid: i32) -> Vec<i32> {
    // SAFETY: a null buffer of size 0 only asks for the count.
    let count = unsafe { libc::proc_listchildpids(pid, std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    // Room for children forked between the two calls.
    let mut pids = vec![0i32; count + 16];
    let Ok(bytes) = i32::try_from(pids.len() * size_of::<i32>()) else {
        return Vec::new();
    };
    // SAFETY: `pids` is a live buffer of exactly `bytes` bytes.
    let filled = unsafe { libc::proc_listchildpids(pid, pids.as_mut_ptr().cast(), bytes) };
    let Ok(filled) = usize::try_from(filled) else {
        return Vec::new();
    };
    pids.truncate(filled.min(pids.len()));
    pids.retain(|&child| child > 0);
    pids
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const fn child_pids(_pid: i32) -> Vec<i32> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::outside_group;

    /// shell 1 → leader 2 (group 2) → helper 3 (group 2) and detached 4 (group 4).
    fn children(pid: i32) -> Vec<i32> {
        match pid {
            1 => vec![2],
            2 => vec![3, 4],
            _ => vec![],
        }
    }

    #[test]
    fn only_a_process_outside_the_foreground_group_is_background() {
        let detached = |pid| Some(if pid == 4 { 4 } else { 2 });
        assert!(outside_group(1, 2, children, detached));
        let attached = |_| Some(2);
        assert!(!outside_group(1, 2, children, attached));
        // A process that vanished mid-walk is no evidence.
        assert!(!outside_group(1, 2, children, |pid| (pid != 4).then_some(2)));
    }
}
