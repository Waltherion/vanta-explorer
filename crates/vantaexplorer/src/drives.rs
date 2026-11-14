//! Mounted filesystems: /proc/self/mountinfo + statvfs. Used by the status bar (used/total/%)
//! and by the sidebar's Mounted disks section.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::time::Duration;

const REFRESH: Duration = Duration::from_secs(5);

/// What the underlying hardware is, for icon selection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DriveKind {
    Nvme,
    Ssd,
    Hdd,
    Usb,
    Optical,
    Unknown,
}

impl DriveKind {
    /// Icon candidates, most specific first. Breeze ships none of the specific ones today, so
    /// these fall through to drive-harddisk, but dropping a custom `drive-harddisk-nvme.svg`
    /// into your icon theme later lights it up with no code change.
    pub fn icon_names(self) -> &'static [&'static str] {
        match self {
            DriveKind::Nvme => &[
                "drive-harddisk-nvme",
                "drive-harddisk-solidstate",
                "drive-harddisk",
            ],
            DriveKind::Ssd => &["drive-harddisk-solidstate", "drive-harddisk"],
            DriveKind::Hdd => &["drive-harddisk"],
            DriveKind::Usb => &[
                "drive-removable-media-usb",
                "drive-removable-media",
                "drive-harddisk",
            ],
            DriveKind::Optical => &["drive-optical", "drive-harddisk"],
            DriveKind::Unknown => &["drive-harddisk"],
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct Mount {
    pub mountpoint: PathBuf,
    pub device: String,
    #[allow(dead_code)]
    pub fstype: String,
    pub label: Option<String>,
    pub used: u64,
    pub total: u64,
    pub kind: DriveKind,
    /// Filesystem UUID, the stable key for the per-drive icon override. Mountpoints
    /// and device names both move; UUIDs do not.
    #[allow(dead_code)]
    pub uuid: Option<String>,
    /// Safe to offer an eject button. Deliberately STRICTER than `DriveKind::Usb`: that one
    /// is an OR of "under a USB controller" and the sysfs removable flag, so an internal
    /// card reader could trip it. This requires all three of: the device sits under a USB
    /// controller, sysfs reports it hot-pluggable, AND its mountpoint is not in /etc/fstab.
    /// A drive somebody wrote into fstab is part of the machine, whatever bus it is on.
    /// A bootable USB stick left in a port is exactly that case, and must never get the button.
    pub ejectable: bool,
}

impl Mount {
    pub fn percent_used(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            self.used as f32 / self.total as f32 * 100.0
        }
    }

    /// Display name, Dolphin-style: filesystem label when set, otherwise "X GiB Drive (dev)".
    pub fn display_name(&self) -> String {
        if let Some(l) = &self.label {
            l.clone()
        } else {
            let dev = self.device.rsplit('/').next().unwrap_or(&self.device);
            format!("{} Drive ({})", crate::model::human_size(self.total), dev)
        }
    }
}

/// Refreshes mount info on a background thread: statvfs can block (network/spun-down disks)
/// and must not sit in the input path.
pub struct Drives {
    /// Result of an eject in flight; None when idle. Also the "one at a time" guard.
    eject_rx: Option<Receiver<Result<String, String>>>,
    mounts: Vec<Mount>,
    rx: Receiver<Vec<Mount>>,
}

impl Drives {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        std::thread::Builder::new()
            .name("vx-drives".into())
            .spawn(move || loop {
                if tx.send(scan()).is_err() {
                    return;
                }
                std::thread::sleep(REFRESH);
            })
            .expect("spawn drives thread");
        Self {
            eject_rx: None,
            mounts: Vec::new(),
            rx,
        }
    }

    /// Non-blocking drain.
    /// Returns true when the mount list changed, so the caller knows to redraw.
    ///
    /// It used to return nothing, and the caller therefore never marked the frame dirty: a
    /// USB stick appearing or being unmounted did not show up until some unrelated input
    /// event happened to force a paint. That is the Wayland on-demand-render trap, work
    /// finishing off the UI thread has to ASK for the frame that shows it.
    ///
    /// Compares before storing rather than assuming a message means a change: the scan runs
    /// every couple of seconds and almost always reports exactly what is already on screen.
    /// Treating each poll as a change would repaint the window twice a second forever.
    /// Kick off an eject. Ignored while one is already running.
    pub fn eject(&mut self, device: String) {
        if self.eject_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.eject_rx = Some(rx);
        std::thread::Builder::new()
            .name("vx-eject".into())
            .spawn(move || {
                let _ = tx.send(run_eject(&device));
            })
            .expect("spawn eject thread");
    }

    /// The finished eject, if one just completed. Clears the in-flight guard.
    pub fn take_eject_result(&mut self) -> Option<Result<String, String>> {
        let done = self.eject_rx.as_ref()?.try_recv().ok()?;
        self.eject_rx = None;
        Some(done)
    }

    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok(v) = self.rx.try_recv() {
            if v != self.mounts {
                self.mounts = v;
                changed = true;
            }
        }
        changed
    }

    /// The mount containing `path` (longest mountpoint prefix wins).
    pub fn mount_for(&self, path: &Path) -> Option<&Mount> {
        self.mounts
            .iter()
            .filter(|m| path.starts_with(&m.mountpoint))
            .max_by_key(|m| m.mountpoint.as_os_str().len())
    }

    #[allow(dead_code)]
    pub fn mounts(&self) -> &[Mount] {
        &self.mounts
    }
}

fn scan() -> Vec<Mount> {
    let labels = read_by_id("/dev/disk/by-label");
    let uuids = read_by_id("/dev/disk/by-uuid");
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let fstab_mountpoints = fstab_mountpoints();
    let mut mounts: Vec<Mount> = Vec::new();
    for line in info.lines() {
        // mountinfo: ... mountpoint ... - fstype source superopts
        let mut halves = line.splitn(2, " - ");
        let left = halves.next().unwrap_or("");
        let right = halves.next().unwrap_or("");
        let mountpoint = match left.split(' ').nth(4) {
            Some(m) => PathBuf::from(unescape(m)),
            None => continue,
        };
        let mut rparts = right.split(' ');
        let fstype = rparts.next().unwrap_or("").to_string();
        let device = rparts.next().unwrap_or("").to_string();

        // Real block devices only; FUSE and network mounts are the sidebar's Remote section.
        if !device.starts_with("/dev/") {
            continue;
        }
        if mountpoint.starts_with("/boot") || fstype == "squashfs" {
            continue;
        }
        // btrfs subvolume mounts of the same device: keep the shortest mountpoint only.
        if mounts
            .iter()
            .any(|m| m.device == device && mountpoint.starts_with(&m.mountpoint))
        {
            continue;
        }

        let Some((used, total)) = statvfs(&mountpoint) else {
            continue;
        };
        let label = labels
            .iter()
            .find(|(dev, _)| *dev == device)
            .map(|(_, l)| l.clone());
        let uuid = uuids
            .iter()
            .find(|(dev, _)| *dev == device)
            .map(|(_, u)| u.clone());
        let kind = drive_kind(&device);
        let ejectable = is_ejectable(&device) && !fstab_mountpoints.contains(&mountpoint);
        mounts.push(Mount {
            mountpoint,
            device,
            fstype,
            label,
            used,
            total,
            kind,
            uuid,
            ejectable,
        });
    }
    mounts.sort_by(|a, b| a.mountpoint.cmp(&b.mountpoint));
    mounts
}

/// Mountpoints declared in /etc/fstab. Matching on the mountpoint rather than parsing
/// UUID=/LABEL=/device spellings keeps this robust to however the entry was written.
fn fstab_mountpoints() -> Vec<PathBuf> {
    std::fs::read_to_string("/etc/fstab")
        .map(|t| {
            t.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .filter_map(|l| l.split_whitespace().nth(1).map(PathBuf::from))
                .collect()
        })
        .unwrap_or_default()
}

/// Partition -> parent disk name: nvme0n1p3 -> nvme0n1, sdb1 -> sdb.
fn parent_disk(device: &str) -> String {
    let dev = device.rsplit('/').next().unwrap_or(device);
    if dev.starts_with("nvme") {
        dev.rsplit_once('p')
            .map(|(d, _)| d.to_string())
            .unwrap_or_else(|| dev.to_string())
    } else {
        dev.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
    }
}

/// Bus and hotplug half of the eject test; the fstab half is applied by the caller.
fn is_ejectable(device: &str) -> bool {
    let base = Path::new("/sys/block").join(parent_disk(device));
    let under_usb = std::fs::canonicalize(base.join("device"))
        .map(|p| p.to_string_lossy().contains("/usb"))
        .unwrap_or(false);
    // udev exposes hotplug as a device attribute rather than a sysfs file; the "removable"
    // flag is the closest thing sysfs itself offers, and for USB sticks the two agree.
    let hotplug = std::fs::read_to_string(base.join("removable"))
        .ok()
        .and_then(|t| t.trim().parse::<u32>().ok())
        == Some(1);
    under_usb && hotplug
}

/// Unmount and power a removable device down, off the UI thread, udisks can take a few
/// seconds flushing. The Ok payload is the device name, for the toast.
fn run_eject(device: &str) -> Result<String, String> {
    let run = |args: &[&str]| -> Result<(), String> {
        let out = std::process::Command::new("udisksctl")
            .args(args)
            .output()
            .map_err(|e| format!("udisksctl: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    };
    run(&["unmount", "-b", device])?;
    // power-off wants the drive, not the partition, so it spins the whole stick down.
    let disk = format!("/dev/{}", parent_disk(device));
    run(&["power-off", "-b", &disk])?;
    Ok(device.to_string())
}

fn statvfs(path: &Path) -> Option<(u64, u64)> {
    let st = rustix::fs::statvfs(path).ok()?;
    let total = st.f_blocks * st.f_frsize;
    let free = st.f_bfree * st.f_frsize;
    Some((total.saturating_sub(free), total))
}

/// Classify a partition device by walking sysfs to its parent disk.
fn drive_kind(device: &str) -> DriveKind {
    let dev = device.rsplit('/').next().unwrap_or(device);
    // Partition -> parent disk: nvme0n1p3 -> nvme0n1, sda2 -> sda.
    let disk = if dev.starts_with("nvme") {
        dev.rsplit_once('p')
            .map(|(d, _)| d.to_string())
            .unwrap_or_else(|| dev.to_string())
    } else {
        dev.trim_end_matches(|c: char| c.is_ascii_digit())
            .to_string()
    };
    let base = Path::new("/sys/block").join(&disk);
    if !base.exists() {
        return DriveKind::Unknown;
    }
    let read_flag = |name: &str| -> Option<u32> {
        std::fs::read_to_string(base.join(name))
            .ok()?
            .trim()
            .parse()
            .ok()
    };
    // USB wins over the rotational flag: a USB SSD is still a removable stick to the user.
    let is_usb = std::fs::canonicalize(base.join("device"))
        .map(|p| p.to_string_lossy().contains("/usb"))
        .unwrap_or(false);
    if is_usb || read_flag("removable") == Some(1) {
        return DriveKind::Usb;
    }
    if disk.starts_with("sr") {
        return DriveKind::Optical;
    }
    if disk.starts_with("nvme") {
        return DriveKind::Nvme;
    }
    match read_flag("queue/rotational") {
        Some(0) => DriveKind::Ssd,
        Some(1) => DriveKind::Hdd,
        _ => DriveKind::Unknown,
    }
}

/// /dev/disk/by-{label,uuid}/<NAME> -> /dev/nvme0n1p3 map.
fn read_by_id(dir: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().replace("\\x20", " ");
        if let Ok(target) = std::fs::canonicalize(e.path()) {
            out.push((target.to_string_lossy().into_owned(), name));
        }
    }
    out
}

/// mountinfo escapes space/tab/newline/backslash as octal.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            let oct: String = chars.by_ref().take(3).collect();
            if let Ok(v) = u8::from_str_radix(&oct, 8) {
                out.push(v as char);
                continue;
            }
            out.push(c);
            out.push_str(&oct);
        } else {
            out.push(c);
        }
    }
    out
}
