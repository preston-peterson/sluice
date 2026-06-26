//! Container attribution (task #1): map a pid to the container it runs in, by parsing
//! `/proc/<pid>/cgroup` (the container id is in the cgroup path). For **Docker** the friendly
//! name is resolved from `/var/lib/docker/containers/<id>/config.v2.json` (root-readable, no
//! Docker socket, no extra deps) and cached. Other runtimes fall back to `runtime:shortid`.

use std::collections::HashMap;

#[derive(Clone, Copy)]
enum Runtime {
    Docker,
    Podman,
    Containerd,
    Nspawn,
}

/// Resolves + caches container labels per pid's cgroup id.
#[derive(Default)]
pub struct Containers {
    /// docker container id → resolved name (None = looked up, no name found).
    docker_names: HashMap<String, Option<String>>,
}

impl Containers {
    /// A display label for the container `pid` runs in, or `None` if it isn't containerized.
    pub fn label(&mut self, pid: u32) -> Option<String> {
        let (rt, id) = detect(&cgroup_path(pid)?)?;
        Some(match rt {
            Runtime::Docker => self
                .docker_name(&id)
                .unwrap_or_else(|| format!("docker:{}", short(&id))),
            Runtime::Podman => format!("podman:{}", short(&id)),
            Runtime::Containerd => format!("containerd:{}", short(&id)),
            Runtime::Nspawn => id, // the machine name is the id here
        })
    }

    fn docker_name(&mut self, id: &str) -> Option<String> {
        if let Some(cached) = self.docker_names.get(id) {
            return cached.clone();
        }
        let name = read_docker_name(id);
        self.docker_names.insert(id.to_string(), name.clone());
        name
    }
}

/// The cgroup path for a pid (works for cgroup v2 `0::/path` and v1 `N:ctrl:/path` lines —
/// we take the path after the last colon and scan it).
fn cgroup_path(pid: u32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    // Prefer a line that mentions a known container marker; else the first line's path.
    for line in s.lines() {
        let path = line.rsplit(':').next().unwrap_or(line);
        if path.contains("docker")
            || path.contains("libpod")
            || path.contains("containerd")
            || path.contains("crio")
            || path.contains("machine-")
        {
            return Some(path.to_string());
        }
    }
    None
}

/// Detect the runtime + container id/name from a cgroup path.
fn detect(path: &str) -> Option<(Runtime, String)> {
    if let Some(id) = between(path, "docker-", ".scope") {
        return Some((Runtime::Docker, id));
    }
    if let Some(id) = after(path, "/docker/") {
        return Some((Runtime::Docker, id));
    }
    if let Some(id) = between(path, "libpod-", ".scope").or_else(|| after(path, "/libpod-")) {
        return Some((Runtime::Podman, id));
    }
    if let Some(id) = between(path, "cri-containerd-", ".scope")
        .or_else(|| between(path, "containerd-", ".scope"))
    {
        return Some((Runtime::Containerd, id));
    }
    if let Some(id) = between(path, "crio-", ".scope") {
        return Some((Runtime::Containerd, id));
    }
    if let Some(name) = between(path, "machine-", ".scope") {
        return Some((Runtime::Nspawn, name.replace('\\', "")));
    }
    None
}

/// Read the Docker container's friendly name from its on-disk metadata.
fn read_docker_name(id: &str) -> Option<String> {
    let path = format!("/var/lib/docker/containers/{id}/config.v2.json");
    let data = std::fs::read_to_string(path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&data).ok()?;
    let name = v.get("Name")?.as_str()?.trim_start_matches('/').trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Substring strictly between `pre` and the next `suf` after it.
fn between(s: &str, pre: &str, suf: &str) -> Option<String> {
    let start = s.find(pre)? + pre.len();
    let rest = &s[start..];
    let end = rest.find(suf)?;
    Some(rest[..end].to_string())
}

/// The path segment immediately following `pre` (up to the next `/`).
fn after(s: &str, pre: &str) -> Option<String> {
    let start = s.find(pre)? + pre.len();
    let seg = s[start..].split('/').next().unwrap_or("");
    if seg.is_empty() {
        None
    } else {
        Some(seg.to_string())
    }
}

/// First 12 chars of a (hex) id, for display.
fn short(id: &str) -> String {
    id.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(rt_path: &str) -> Option<String> {
        detect(rt_path).map(|(_, id)| id)
    }

    #[test]
    fn docker_scope_v2() {
        // The real cgroup line seen on the host (systemd cgroup driver, v2).
        let p = "/system.slice/docker-558e814e04c93a38f352dd37c5884eabdee389bf5088a4ef3fcadd93c4ade02e.scope";
        assert_eq!(
            id(p).as_deref(),
            Some("558e814e04c93a38f352dd37c5884eabdee389bf5088a4ef3fcadd93c4ade02e")
        );
    }

    #[test]
    fn docker_v1_path() {
        assert_eq!(id("/docker/abc123def456").as_deref(), Some("abc123def456"));
    }

    #[test]
    fn nspawn_machine() {
        assert_eq!(
            id("/machine.slice/machine-web.scope").as_deref(),
            Some("web")
        );
    }

    #[test]
    fn not_a_container() {
        assert!(detect("/user.slice/user-1000.slice/session-2.scope").is_none());
    }

    #[test]
    fn short_truncates() {
        assert_eq!(short("0123456789abcdef0000"), "0123456789ab");
    }
}
