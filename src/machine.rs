//! System probes for the launch banner.
//!
//! Each probe is one syscall or one small file read, except [`version`], which
//! has to run the shell: a shell reports its own version and nothing in the
//! environment carries it. Nothing here uses the network. A probe that fails
//! returns `None`, and the banner leaves that line out.

use std::{
    fs,
    process::{Command, Stdio},
};

/// Seconds since boot.
pub fn uptime() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let text = fs::read_to_string("/proc/uptime").ok()?;
        Some(text.split_whitespace().next()?.parse::<f64>().ok()? as u64)
    }
    #[cfg(target_os = "macos")]
    {
        // `kern.boottime` is a `struct timeval`; its first field is the epoch
        // second the machine started.
        let boot = sysctl_raw(b"kern.boottime\0")?;
        let since = i64::from_ne_bytes(boot.get(..8)?.try_into().ok()?);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .ok()?
            .as_secs() as i64;
        Some((now - since).max(0) as u64)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The operating system and its version: "NixOS 25.05", "macOS 15.2".
pub fn system() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let text = fs::read_to_string("/etc/os-release").ok()?;
        let value = |key: &str| {
            text.lines()
                .find(|line| line.starts_with(key))
                .and_then(|line| line.split_once('='))
                .map(|(_, value)| value.trim_matches('"').to_owned())
        };
        let name = value("PRETTY_NAME=").or_else(|| value("NAME="))?;
        if name.is_empty() {
            return None;
        }
        // PRETTY_NAME usually carries the version already; NAME does not, so add
        // VERSION_ID when the name has no version in it.
        match value("VERSION_ID").filter(|version| !version.is_empty() && !name.contains(version)) {
            Some(version) => Some(format!("{name} {version}")),
            None => Some(name),
        }
    }
    #[cfg(target_os = "macos")]
    {
        let version = sysctl_string(b"kern.osproductversion\0").or_else(|| {
            plist_value(
                "/System/Library/CoreServices/SystemVersion.plist",
                "ProductVersion",
            )
        })?;
        Some(format!("macOS {version}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The kernel and its release, as `uname` reports them: "Linux 6.12.9".
pub fn kernel() -> Option<String> {
    // SAFETY: zeroed bytes are a valid `utsname`, and `uname` fills it in.
    let mut name = unsafe { std::mem::zeroed::<libc::utsname>() };
    if unsafe { libc::uname(&mut name) } != 0 {
        return None;
    }
    let field = |value: &[libc::c_char]| {
        let bytes: Vec<u8> = value
            .iter()
            .take_while(|&&byte| byte != 0)
            .map(|&byte| byte.to_ne_bytes()[0])
            .collect();
        String::from_utf8(bytes).ok()
    };
    Some(format!(
        "{} {}",
        field(&name.sysname)?,
        field(&name.release)?
    ))
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    // SAFETY: `buf` is valid writable storage for its full length.
    if unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) } == 0 {
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        if let Ok(name) = std::str::from_utf8(&buf[..end])
            && !name.trim().is_empty()
        {
            return name.trim().to_owned();
        }
    }
    std::env::var("HOSTNAME")
        .ok()
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "localhost".into())
}

pub fn user() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .unwrap_or_else(|| "someone".into())
}

/// The processor: its brand, its speed when the kernel will say, and how many
/// of it are awake.
///
/// Linux keeps those in three different places, and a machine that reports
/// neither brand nor speed answers nothing rather than a bare "8 cores".
pub fn cpu() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let text = fs::read_to_string("/proc/cpuinfo").ok()?;
        let field = |key: &str| {
            text.lines()
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.trim() == key)
                .map(|(_, value)| value.trim().to_owned())
        };
        let model = field("model name")?;
        let cores = text
            .lines()
            .filter(|line| line.starts_with("processor"))
            .count();
        let speed = field("mhz").or_else(|| field("cpu mhz"));
        let detail = match (speed, cores) {
            (Some(speed), 0) => format!("{speed} GHz"),
            (Some(speed), cores) => format!("{speed} GHz, {cores} cores"),
            // A brand alone is not worth a row.
            (None, 0) => return None,
            (None, cores) => format!("{cores} cores"),
        };
        Some(format!("{model}, {detail}"))
    }
    #[cfg(target_os = "macos")]
    {
        let brand = sysctl_string(b"machdep.cpu.brand_string\0")?;
        let cores = sysctl_string(b"hw.ncpu\0").unwrap_or_default();
        let speed = sysctl_string(b"hw.cpufrequency\0")
            .and_then(|hz| hz.parse::<u64>().ok())
            .map(|hz| format!("{:.1} GHz", hz as f64 / 1e9))
            .filter(|speed| !speed.starts_with("0"));
        let detail = match (speed, cores.is_empty()) {
            (Some(speed), false) => format!("{speed}, {cores} cores"),
            (Some(speed), true) => speed,
            (None, false) => format!("{cores} cores"),
            (None, true) => return None,
        };
        Some(format!("{brand}, {detail}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// The installed memory, in gigabytes: "32 GB".
///
/// The kernel gives an exact count, but machines are sold by the gigabyte, so
/// the count is rounded to the nearest one: 4097 MiB is the same machine as
/// 4096 MiB.
pub fn memory() -> Option<String> {
    #[cfg(target_os = "linux")]
    let kib = || {
        let text = fs::read_to_string("/proc/meminfo").ok()?;
        let value = text
            .lines()
            .find_map(|line| line.split_once(':'))
            .filter(|(name, _)| name.trim() == "MemTotal")?;
        value.1.split_whitespace().next()?.parse::<u64>().ok()
    };
    #[cfg(target_os = "macos")]
    let kib = || {
        let bytes = sysctl_raw(b"hw.memsize\0")?.get(..8)?.try_into().ok()?;
        Some(u64::from_ne_bytes(bytes) / 1024)
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let kib = || None;

    Some(format_memory(kib()?))
}

/// KiB, written at the largest unit it reaches: a size under a gigabyte keeps
/// its megabytes instead of rounding to zero.
fn format_memory(kib: u64) -> String {
    let mib = kib / 1024;
    if mib < 1024 {
        return if mib == 0 {
            format!("{kib} KB")
        } else {
            format!("{mib} MB")
        };
    }
    format!("{} GB", (mib + 512) / 1024)
}

/// The terminal program and its version: "Ghostty 1.4.0".
///
/// A terminal that names itself exports both variables, so this costs nothing.
pub fn terminal() -> Option<String> {
    let name = std::env::var("TERM_PROGRAM").ok()?;
    if name.is_empty() || name == "dumb" {
        return None;
    }
    let version = std::env::var("TERM_PROGRAM_VERSION")
        .ok()
        .map(|version| trim_v(&version))
        .filter(|version| !version.is_empty());
    Some(match version {
        Some(version) => format!("{} {version}", title_case(&name)),
        None => title_case(&name),
    })
}

/// Ask a program for its own version.
///
/// `--version` is the only reliable way to get a shell's version. stdin is
/// /dev/null so a program that ignores the flag and starts reading gets EOF and
/// exits, rather than blocking on the terminal.
pub fn version(program: &str) -> Option<String> {
    let output = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = if stdout.trim().is_empty() {
        String::from_utf8_lossy(&output.stderr).into_owned()
    } else {
        stdout.into_owned()
    };
    semantic_version(text.lines().next()?)
}

/// The first `x.y` or `x.y.z` in a version banner.
///
/// Handles "Nushell 0.116.1", "GNU bash, version 5.2.32(1)-release
/// (x86_64-pc-linux)" and "fish, version 4.1.2". Returns `None` when the line
/// carries no dotted number.
pub fn semantic_version(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() {
            at += 1;
            continue;
        }
        let mut end = at;
        let mut dotted = false;
        while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == b'.') {
            if bytes[end] == b'.' {
                // A dot only belongs to the number if another digit follows.
                if end + 1 >= bytes.len() || !bytes[end + 1].is_ascii_digit() {
                    break;
                }
                dotted = true;
            }
            end += 1;
        }
        if dotted {
            return Some(text[at..end].to_owned());
        }
        at = end.max(at + 1);
    }
    None
}

/// A leading `v` is a tag, not part of the version.
fn trim_v(version: &str) -> String {
    version.trim().trim_start_matches('v').to_owned()
}

/// "3 days, 4 hrs" for long uptimes, "47 secs" for short ones.
pub fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = seconds % 86_400 / 3_600;
    let minutes = seconds % 3_600 / 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{} day{}", days, if days == 1 { "" } else { "s" }));
    }
    if hours > 0 {
        parts.push(format!("{hours} hr{}", if hours == 1 { "" } else { "s" }));
    }
    if days == 0 && minutes > 0 {
        parts.push(format!("{minutes} min"));
    }
    if parts.is_empty() {
        let shown = seconds.max(1);
        return format!("{shown} sec{}", if shown == 1 { "" } else { "s" });
    }
    parts.join(", ")
}

fn title_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper = true;
    for c in name.chars() {
        if c == '-' || c == '_' {
            out.push('-');
            upper = true;
            continue;
        }
        out.push(if upper {
            c.to_ascii_uppercase()
        } else {
            c.to_ascii_lowercase()
        });
        upper = false;
    }
    out
}

/// The `<string>` that follows a `<key>` in an Apple plist.
#[cfg(target_os = "macos")]
fn plist_value(path: &str, key: &str) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines().map(str::trim);
    while let Some(line) = lines.next() {
        if line == format!("<key>{key}</key>") {
            return lines
                .next()?
                .strip_prefix("<string>")?
                .strip_suffix("</string>")
                .map(str::to_owned);
        }
    }
    None
}

/// `sysctl` for the string kinds (`kern.osproductversion` and friends).
#[cfg(target_os = "macos")]
fn sysctl_string(name: &[u8]) -> Option<String> {
    let mut bytes = sysctl_raw(name)?;
    bytes.pop(); // the NUL that ends a string kind
    String::from_utf8(bytes)
        .ok()
        .filter(|value| !value.is_empty())
}

/// Ask the kernel for one name's value, in bytes. The length is asked for first,
/// because `kern.boottime` is a struct and `kern.osproductversion` is a string.
#[cfg(target_os = "macos")]
fn sysctl_raw(name: &[u8]) -> Option<Vec<u8>> {
    let name = name.as_ptr().cast::<libc::c_char>();
    let mut len = 0usize;
    // SAFETY: `name` is NUL terminated, and the first call only asks for a size.
    unsafe {
        if libc::sysctlbyname(
            name,
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
            || len == 0
        {
            return None;
        }
        let mut buf = vec![0u8; len];
        if libc::sysctlbyname(
            name,
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        ) != 0
        {
            return None;
        }
        buf.truncate(len);
        Some(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cheap_probes_answer_or_stay_quiet() {
        assert!(!hostname().is_empty());
        assert!(!user().is_empty());
        if let Some(kernel) = kernel() {
            assert!(kernel.contains(' '), "{kernel}");
        }
        assert!(uptime().is_none_or(|seconds| seconds > 0));
    }

    #[test]
    fn version_banners_yield_their_number() {
        assert_eq!(semantic_version("Nushell 0.116.1"), Some("0.116.1".into()));
        assert_eq!(semantic_version("nu"), None);
        assert_eq!(
            semantic_version("GNU bash, version 5.2.32(1)-release (x86_64-pc-linux-gnu)"),
            Some("5.2.32".into())
        );
        assert_eq!(
            semantic_version("fish, version 4.1.2"),
            Some("4.1.2".into())
        );
        assert_eq!(
            semantic_version("zsh 5.9 (x86_64-unknown)"),
            Some("5.9".into())
        );
        assert_eq!(semantic_version("Ghostty 1.4.0"), Some("1.4.0".into()));
        assert_eq!(semantic_version("version 1."), None);
        assert_eq!(trim_v("  v1.4.0 "), "1.4.0");
    }

    #[test]
    fn a_real_program_answers_or_says_nothing() {
        // /bin/sh is present on every platform haunt builds on, and whether it
        // reports a version is up to it.
        if let Some(text) = version("/bin/sh") {
            assert!(text.contains('.'), "{text}");
        }
        assert_eq!(version("/definitely/not/a/program"), None);
    }

    #[test]
    fn uptime_is_formatted_by_size() {
        assert_eq!(format_uptime(0), "1 sec");
        assert_eq!(format_uptime(1), "1 sec");
        assert_eq!(format_uptime(2), "2 secs");
        assert_eq!(format_uptime(47), "47 secs");
        assert_eq!(format_uptime(90), "1 min");
        assert_eq!(format_uptime(210), "3 min");
        assert_eq!(format_uptime(3_600), "1 hr");
        assert_eq!(format_uptime(7 * 3_600), "7 hrs");
        assert_eq!(
            format_uptime(3 * 86_400 + 4 * 3_600 + 12 * 60),
            "3 days, 4 hrs"
        );
        assert_eq!(format_uptime(86_400 + 60), "1 day");
    }

    #[test]
    fn memory_is_written_at_the_unit_it_reaches() {
        assert_eq!(format_memory(512), "512 KB");
        assert_eq!(format_memory(2048), "2 MB");
        assert_eq!(format_memory(1023 * 1024), "1023 MB");
        // A real size is exact, and the rounding of a little slack never invents
        // a capacity the machine does not have.
        assert_eq!(format_memory(1024 * 1024), "1 GB");
        assert_eq!(format_memory(4 * 1024 * 1024), "4 GB");
        assert_eq!(format_memory(4 * 1024 * 1024 + 1), "4 GB");
        assert_eq!(format_memory(3800 * 1024), "4 GB");
        assert_eq!(format_memory(12 * 1024 * 1024), "12 GB");
    }

    #[test]
    fn terminal_names_are_title_cased() {
        assert_eq!(title_case("ghostty"), "Ghostty");
        assert_eq!(title_case("apple_terminal"), "Apple-Terminal");
    }
}
