//! `COMSPEC` for the X server bundled with the Windows app (VcXsrv).
//!
//! VcXsrv compiles its keymap with the C runtime's `system()`, which runs
//! `%COMSPEC% /c <command>`. VcXsrv is a GUI program without a console, so every such cmd.exe
//! got a console window of its own (Windows Terminal's, on Windows 11) that flashed up as X11
//! forwarding started. Muxus starts VcXsrv with COMSPEC naming this program, a GUI program too,
//! which runs the same command through cmd.exe without a console window and hands back its exit
//! code.

// No console window of its own, in debug builds too.
#![windows_subsystem = "windows"]

fn main() {
    #[cfg(windows)]
    std::process::exit(hidden::run());
}

#[cfg(windows)]
mod hidden {
    use std::os::windows::process::CommandExt;
    use std::path::PathBuf;
    use std::process::Command;

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCommandLineW() -> *const u16;
    }

    /// The command line as the caller wrote it: cmd.exe's own quoting rules apply to what
    /// follows `/c`, which the standard argument parsing would undo.
    fn command_line() -> String {
        // SAFETY: GetCommandLineW returns a NUL-terminated string that lives as long as the
        // process.
        unsafe {
            let line = GetCommandLineW();
            let mut length = 0;
            while *line.add(length) != 0 {
                length += 1;
            }
            String::from_utf16_lossy(std::slice::from_raw_parts(line, length))
        }
    }

    pub fn run() -> i32 {
        let line = command_line();
        let Some(arguments) = super::cmd_arguments(&line) else {
            return 1;
        };
        let cmd = std::env::var_os("SystemRoot")
            .map(|root| PathBuf::from(root).join("System32").join("cmd.exe"))
            .unwrap_or_else(|| "cmd.exe".into());
        match Command::new(cmd)
            .raw_arg(arguments)
            .creation_flags(CREATE_NO_WINDOW)
            .status()
        {
            Ok(status) => status.code().unwrap_or(1),
            Err(_) => 1,
        }
    }
}

/// What to hand cmd.exe: everything from `/c` on. The C runtime puts this program's path in
/// front unquoted, spaces and all, but a Windows path never holds a `/`, so the first one starts
/// the switch.
#[cfg_attr(not(windows), allow(dead_code))]
fn cmd_arguments(line: &str) -> Option<&str> {
    let rest = &line[line.find('/')?..];
    let switch = rest.get(..2)?;
    (switch.eq_ignore_ascii_case("/c") || switch.eq_ignore_ascii_case("/k")).then_some(rest)
}

#[cfg(test)]
mod tests {
    use super::cmd_arguments;

    #[test]
    fn hands_cmd_everything_from_the_switch_on() {
        let command = r#"/c ""C:\vcxsrv\xkbcomp" -w 1 "-RC:\vcxsrv\xkbdata" -xkm "C:\Temp\xkb_a1" "C:\Temp\server-10.xkm"""#;
        for program in [
            r"C:\Program Files\Muxus\muxus-hidden-cmd.exe",
            r#""C:\Program Files\Muxus\muxus-hidden-cmd.exe""#,
        ] {
            assert_eq!(
                cmd_arguments(&format!("{program} {command}")),
                Some(command)
            );
        }
        assert_eq!(cmd_arguments(r"C:\muxus-hidden-cmd.exe"), None);
        assert_eq!(cmd_arguments(r"C:\muxus-hidden-cmd.exe /q dir"), None);
    }
}
