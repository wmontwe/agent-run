//! The gate's report: what a container sees, as the launcher parses it. The gate prints
//! one line per mount and one per planned target, in the plan's order:
//!
//! ```text
//! mount <line of /proc/self/mountinfo>
//! target <index> <dev> <ino>      (or `target <index> - -` if missing)
//! ```

/// One line of `/proc/self/mountinfo`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountInfo {
    pub root: String,
    pub mount_point: String,
    pub options: Vec<String>,
    pub fs_type: String,
}

impl MountInfo {
    pub fn is_read_only(&self) -> bool {
        self.options.iter().any(|o| o == "ro")
    }
}

/// Identity of a planned target as seen inside the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetStat {
    pub dev: u64,
    pub ino: u64,
}

#[derive(Debug, Default)]
pub struct Report {
    pub mounts: Vec<MountInfo>,
    /// Indexed like the plan; `None` if the target doesn't exist.
    pub targets: Vec<Option<TargetStat>>,
}

impl Report {
    /// The mount visible at `path`: the last one in the table.
    pub fn visible(&self, path: &str) -> Option<&MountInfo> {
        self.mounts.iter().rev().find(|m| m.mount_point == path)
    }
}

/// Undoes mountinfo's octal escapes (`\040` for a space, ...).
fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 4 <= bytes.len()
            && let Ok(code) = u8::from_str_radix(&field[i + 1..i + 4], 8)
        {
            out.push(code);
            i += 4;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `id parent major:minor root mount_point options [optional...] - fs_type source super_options`.
fn parse_mountinfo_line(line: &str) -> Option<MountInfo> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    let separator = fields.iter().position(|f| *f == "-")?;
    if separator < 6 || fields.len() < separator + 2 {
        return None;
    }
    Some(MountInfo {
        root: unescape(fields[3]),
        mount_point: unescape(fields[4]),
        options: fields[5].split(',').map(str::to_owned).collect(),
        fs_type: fields[separator + 1].to_owned(),
    })
}

pub fn parse_report(text: &str, target_count: usize) -> Result<Report, String> {
    let mut report = Report {
        mounts: Vec::new(),
        targets: vec![None; target_count],
    };
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("mount ") {
            report
                .mounts
                .push(parse_mountinfo_line(rest).ok_or_else(|| format!("bad mount line: {rest}"))?);
        } else if let Some(rest) = line.strip_prefix("target ") {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            let [index, dev, ino] = parts[..] else {
                return Err(format!("bad target line: {rest}"));
            };
            let index: usize = index
                .parse()
                .map_err(|_| format!("bad target index: {index}"))?;
            if index >= target_count {
                return Err(format!("target index out of range: {index}"));
            }
            if dev != "-" {
                let dev = dev.parse().map_err(|_| format!("bad dev: {dev}"))?;
                let ino = ino.parse().map_err(|_| format!("bad ino: {ino}"))?;
                report.targets[index] = Some(TargetStat { dev, ino });
            }
        } else if !line.is_empty() {
            return Err(format!("unexpected report line: {line}"));
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mountinfo_with_optional_fields_and_escapes() {
        let line = "1234 1200 0:52 /Users/me/My\\040Project /Users/me/My\\040Project ro,relatime shared:1 master:2 - virtiofs a rw";
        let mount = parse_mountinfo_line(line).unwrap();
        assert_eq!(mount.root, "/Users/me/My Project");
        assert_eq!(mount.mount_point, "/Users/me/My Project");
        assert!(mount.is_read_only());
        assert_eq!(mount.fs_type, "virtiofs");
    }

    #[test]
    fn parses_targets_and_missing_ones() {
        let report = parse_report("target 0 64 99\ntarget 1 - -\n", 2).unwrap();
        assert_eq!(
            report.targets,
            vec![Some(TargetStat { dev: 64, ino: 99 }), None]
        );
    }

    #[test]
    fn rejects_unexpected_lines_and_indexes() {
        assert!(parse_report("hello\n", 1).is_err());
        assert!(parse_report("target 5 1 1\n", 1).is_err());
        assert!(parse_report("mount garbage\n", 1).is_err());
    }
}
