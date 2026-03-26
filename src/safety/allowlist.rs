use std::collections::HashSet;

/// Static allowlist of known-safe read-only commands.
/// These are auto-approved without Claude AI review.
pub struct Allowlist {
    safe_binaries: HashSet<&'static str>,
}

impl Allowlist {
    pub fn new() -> Self {
        let safe_binaries: HashSet<&str> = [
            "ls",
            "cat",
            "head",
            "tail",
            "grep",
            "egrep",
            "fgrep",
            "rg",
            "ps",
            "df",
            "du",
            "top",
            "htop",
            "free",
            "uptime",
            "uname",
            "hostname",
            "whoami",
            "id",
            "date",
            "cal",
            "file",
            "stat",
            "wc",
            "sort",
            "uniq",
            "diff",
            "comm",
            "find",
            "which",
            "whereis",
            "type",
            "git",
            "pwd",
            "env",
            "printenv",
            "dig",
            "nslookup",
            "host",
            "ping",
            "traceroute",
            "ss",
            "netstat",
            "ip",
            "journalctl",
            "dmesg",
            "lsof",
            "lsblk",
            "lscpu",
            "lsmem",
            "lspci",
            "mount", // read-only when no args that modify
            "test",
            "[",
        ]
        .into_iter()
        .collect();

        Self { safe_binaries }
    }

    /// Check if a command is in the safe allowlist.
    /// Only the binary name is checked — args are not analyzed.
    /// The Guardian AI handles nuanced arg checking.
    pub fn is_safe(&self, binary: &str) -> bool {
        self.safe_binaries.contains(binary)
    }

    /// Check if a tool action is inherently safe (read-only operations).
    pub fn is_safe_tool_action(tool_name: &str, action: &str) -> bool {
        matches!(
            (tool_name, action),
            ("file_ops", "read" | "list" | "exists")
                | ("log_reader", "read" | "search")
                | ("code_writer", _) // code_writer is never auto-safe
        ) && !matches!((tool_name, action), ("code_writer", _))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_read_commands_are_safe() {
        let al = Allowlist::new();
        assert!(al.is_safe("ls"));
        assert!(al.is_safe("cat"));
        assert!(al.is_safe("grep"));
        assert!(al.is_safe("ps"));
        assert!(al.is_safe("df"));
        assert!(al.is_safe("git"));
    }

    #[test]
    fn dangerous_commands_not_safe() {
        let al = Allowlist::new();
        assert!(!al.is_safe("rm"));
        assert!(!al.is_safe("sudo"));
        assert!(!al.is_safe("systemctl"));
        assert!(!al.is_safe("chmod"));
        assert!(!al.is_safe("chown"));
        assert!(!al.is_safe("dd"));
    }

    #[test]
    fn safe_tool_actions() {
        assert!(Allowlist::is_safe_tool_action("file_ops", "read"));
        assert!(Allowlist::is_safe_tool_action("file_ops", "list"));
        assert!(Allowlist::is_safe_tool_action("log_reader", "search"));
        assert!(!Allowlist::is_safe_tool_action("file_ops", "write"));
        assert!(!Allowlist::is_safe_tool_action("file_ops", "delete"));
    }
}
