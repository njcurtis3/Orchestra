//! The service unit's security directives, pinned like `csp_pinned` in the desktop shell:
//! loosening the sandbox is a decision, so it must change this test in the same commit.

const UNIT: &str = include_str!("../systemd/orchestrad.service");
const GPU: &str = include_str!("../systemd/gpu.conf.example");

/// The directive lines of a unit: comments, blank lines and section headers dropped.
fn lines(text: &str) -> Vec<&str> {
    text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('[')).collect()
}

fn has(text: &str, directive: &str) -> bool {
    lines(text).contains(&directive)
}

fn key_present(text: &str, key: &str) -> bool {
    lines(text).iter().any(|l| l.split('=').next() == Some(key))
}

#[test]
fn runs_as_the_orchestra_user() {
    assert!(has(UNIT, "User=orchestra"));
    assert!(has(UNIT, "Group=orchestra"));
    assert!(!has(UNIT, "User=root"));
    assert!(!has(UNIT, "User=0"));
}

#[test]
fn stop_takes_the_whole_cgroup() {
    assert!(has(UNIT, "KillMode=control-group"));
}

#[test]
fn sandbox_directives_are_present() {
    for d in [
        "NoNewPrivileges=yes",
        "ProtectSystem=strict",
        "ProtectHome=yes",
        "PrivateTmp=yes",
        "PrivateDevices=yes",
        "ProtectKernelTunables=yes",
        "ProtectKernelModules=yes",
        "ProtectKernelLogs=yes",
        "ProtectControlGroups=yes",
        "RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6",
        "LockPersonality=yes",
        "RestrictRealtime=yes",
        "SystemCallArchitectures=native",
        "UMask=0077",
    ] {
        assert!(has(UNIT, d), "missing {d}");
    }
}

#[test]
fn capability_bounding_set_is_empty() {
    assert!(has(UNIT, "CapabilityBoundingSet="));
}

#[test]
fn default_unit_opens_no_device() {
    assert!(!key_present(UNIT, "DeviceAllow"));
    assert!(!key_present(UNIT, "SupplementaryGroups"));
}

#[test]
fn nodes_jit_and_user_namespaces_stay_allowed() {
    assert!(!key_present(UNIT, "MemoryDenyWriteExecute"));
    assert!(!key_present(UNIT, "RestrictNamespaces"));
}

#[test]
fn state_and_secrets_paths() {
    assert!(has(UNIT, "StateDirectory=orchestra"));
    assert!(has(UNIT, "StateDirectoryMode=0700"));
    assert!(has(UNIT, "RuntimeDirectory=orchestrad"));
    assert!(has(UNIT, "EnvironmentFile=-/etc/orchestrad/env"));
    assert!(has(UNIT, "ExecStart=/usr/local/bin/orchestrad run --config /etc/orchestrad/config.toml"));
}

#[test]
fn gpu_example_carries_the_device_lines() {
    assert!(has(GPU, "PrivateDevices=no"));
    // DeviceAllow takes device nodes or char-/block- class names; it takes no globs on
    // /dev paths and no directories, so /dev/nvidia* and /dev/dri would be silently dropped.
    assert!(has(GPU, "DeviceAllow=char-nvidia-frontend rw"));
    assert!(has(GPU, "DeviceAllow=char-nvidia-uvm rw"));
    assert!(has(GPU, "DeviceAllow=char-drm rw"));
    // With PrivateDevices=no the allowlist applies, so the pty nodes must be re-allowed.
    assert!(has(GPU, "DeviceAllow=/dev/ptmx rw"));
    assert!(has(GPU, "DeviceAllow=char-pts rw"));
    assert!(!lines(GPU).iter().any(|l| l.starts_with("DeviceAllow=/dev/nvidia") || l.starts_with("DeviceAllow=/dev/dri")));
}

#[test]
fn gpu_example_does_not_make_a_missing_group_fatal() {
    // SupplementaryGroups= naming a group the distro lacks (e.g. render) fails the start.
    assert!(!key_present(GPU, "SupplementaryGroups"));
}
