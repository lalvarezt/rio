//! Shell-facing SSH wrapper. Authentication and terminal I/O belong to OpenSSH.
use super::*;
use std::ffi::OsString;
use std::io::{IsTerminal, Read, Write};

const TERMINFO: &str = include_str!("../../../../misc/rio.terminfo");

/// Direct palette and `rio -e ssh` launches need the same safe terminal defaults
/// as shell wrappers, without injecting local shell startup scripts into ssh.
pub fn prepare_shell(shell: &Shell) -> crate::shell_integration::SpawnIntegration {
    let mut args = vec![
        "-oSendEnv=COLORTERM".into(),
        "-oSendEnv=TERM_PROGRAM".into(),
        "-oSendEnv=TERM_PROGRAM_VERSION".into(),
    ];
    args.extend(shell.args.clone());
    crate::shell_integration::SpawnIntegration {
        env: vec![("TERM".into(), "xterm-256color".into())],
        command: Some((shell.program.clone(), args)),
    }
}

struct Options {
    program: OsString,
    args: Vec<OsString>,
    forward_env: bool,
    terminfo: bool,
    cache: bool,
}

impl Options {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, String> {
        let mut options = Self {
            program: if cfg!(windows) { "ssh.exe" } else { "ssh" }.into(),
            args: Vec::new(),
            forward_env: true,
            terminfo: true,
            cache: true,
        };
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.to_str() {
                Some("--") => {
                    options.args.extend(args);
                    break;
                }
                Some("--forward-env=false") => options.forward_env = false,
                Some("--forward-env" | "--forward-env=true") => {
                    options.forward_env = true
                }
                Some("--terminfo=false") => options.terminfo = false,
                Some("--terminfo" | "--terminfo=true") => options.terminfo = true,
                Some("--cache=false") => options.cache = false,
                Some("--cache" | "--cache=true") => options.cache = true,
                Some("--ssh") => {
                    options.program = args.next().ok_or("--ssh needs a path")?
                }
                Some(value) if value.starts_with("--ssh=") => {
                    options.program = value[6..].into();
                }
                Some(value) if value.starts_with("--") => {
                    return Err(format!("Unknown +ssh option: {value}"));
                }
                _ => {
                    options.args.push(arg);
                    options.args.extend(args);
                    break;
                }
            }
        }
        if options.program.is_empty() {
            return Err("--ssh needs a path".into());
        }
        Ok(options)
    }

    fn command(&self, term: &str) -> Command {
        let mut command = Command::new(&self.program);
        if self.forward_env {
            command.args([
                "-oSendEnv=COLORTERM",
                "-oSendEnv=TERM_PROGRAM",
                "-oSendEnv=TERM_PROGRAM_VERSION",
            ]);
        }
        command.args(&self.args);
        if self.forward_env || self.terminfo {
            command.env("TERM", term);
        }
        command
    }

    fn connection(&self) -> Option<Connection> {
        let args = self
            .args
            .iter()
            .map(|s| s.to_str().map(str::to_owned))
            .collect::<Option<Vec<_>>>()?;
        let mut connection = Connection::from_args(&args)?;
        connection.program = Some(self.program.to_str()?.to_owned());
        Some(connection)
    }
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> i32 {
    let args: Vec<_> = args.into_iter().collect();
    if args.first().is_some_and(|arg| arg == "--help") {
        println!("Usage: rio +ssh [--ssh=PATH] [--terminfo=false] [--cache=false] [--forward-env=false] [--] SSH_ARGS...\n\nRuns native SSH with Rio terminal compatibility. Authentication and host trust use your SSH client and configuration.");
        return 0;
    }
    let options = match Options::parse(args) {
        Ok(options) => options,
        Err(error) => {
            eprintln!("rio +ssh: {error}");
            return 2;
        }
    };
    let connection = options.connection();
    let term = if options.terminfo
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && connection
            .as_ref()
            .is_some_and(|connection| install_terminfo(connection, options.cache))
    {
        "xterm-rio"
    } else {
        "xterm-256color"
    };
    let _registration = connection.as_ref().and_then(Registration::new);
    let mut child = match options.command(term).spawn() {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "rio +ssh: could not start {}: {error}",
                options.program.to_string_lossy()
            );
            return 127;
        }
    };
    // Both processes share the terminal's foreground process group. Let ssh
    // handle Ctrl+C while this supervisor stays alive to clear the recipe.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGQUIT, libc::SIG_IGN);
    }
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Console::SetConsoleCtrlHandler(
            Some(keep_supervisor_alive),
            1,
        );
    }
    match child.wait() {
        Ok(status) => exit_code(status),
        Err(error) => {
            eprintln!("rio +ssh: {error}");
            1
        }
    }
}

#[cfg(windows)]
unsafe extern "system" fn keep_supervisor_alive(event: u32) -> i32 {
    i32::from(event == 0 || event == 1)
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0))
    }
    #[cfg(not(unix))]
    status.code().unwrap_or(1)
}

fn install_terminfo(connection: &Connection, use_cache: bool) -> bool {
    use std::hash::{Hash, Hasher};
    let Ok(settings) = connection.settings() else {
        return false;
    };
    let Some(cache) = dirs::cache_dir().map(|dir| dir.join("rio/ssh-terminfo")) else {
        return false;
    };
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for key in ["user", "hostname", "port"] {
        let Some(value) = setting(&settings, key) else {
            return false;
        };
        value.hash(&mut hash);
    }
    TERMINFO.hash(&mut hash);
    let path = cache.join(format!("{:016x}", hash.finish()));
    if use_cache && path.is_file() {
        return true;
    }
    let mut command = setup_command(connection);
    command.arg("command -v tic >/dev/null 2>&1 && mkdir -p ~/.terminfo && tic -x -");
    let output = capture_output(
        command,
        Some(TERMINFO.as_bytes().to_vec()),
        Duration::from_secs(8),
    );
    if !output.is_ok_and(|output| output.status.success()) {
        return false;
    }
    if use_cache && std::fs::create_dir_all(cache).is_ok() {
        let _ = std::fs::write(path, []);
    }
    true
}

fn setup_command(connection: &Connection) -> Command {
    let mut command = Command::new(connection.program());
    // A setup connection must never replay the user's remote command, forwards,
    // or local command. It is best effort and may not ask for credentials.
    command.args([
        "-oBatchMode=yes",
        "-oConnectTimeout=5",
        "-oConnectionAttempts=1",
        "-oClearAllForwardings=yes",
        "-oPermitLocalCommand=no",
        "-oRemoteCommand=none",
        "-oRequestTTY=no",
        "-oSessionType=default",
        "-oForkAfterAuthentication=no",
        "-oStdinNull=no",
        "-oControlMaster=no",
    ]);
    command
        .args(&connection.options)
        .arg(&connection.destination);
    command
}

/// Local-only pane state. Remote OSC reports cannot supply an SSH recipe.
pub struct PaneSession {
    directory: PathBuf,
}

impl PaneSession {
    pub fn new(route: usize) -> Option<Self> {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let directory = dirs::cache_dir()?
            .join("rio/ssh-panes")
            .join(format!("{}-{route}-{stamp}", std::process::id()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&directory)
                .ok()?;
        }
        #[cfg(not(unix))]
        std::fs::create_dir_all(&directory).ok()?;
        Some(Self { directory })
    }

    pub fn env(&self) -> (String, String) {
        (
            "RIO_SSH_CONTEXT".into(),
            self.directory.to_string_lossy().into_owned(),
        )
    }

    pub fn connection(&self) -> Option<Connection> {
        // Each supervisor owns its own file, so a nested invocation or a crashed
        // helper cannot remove another connection's state.
        let mut active = Vec::new();
        for entry in std::fs::read_dir(&self.directory).ok()?.flatten() {
            let Some(pid) = entry
                .file_name()
                .to_str()
                .and_then(|s| s.parse::<u32>().ok())
            else {
                continue;
            };
            if !process_alive(pid) {
                continue;
            }
            let mut text = String::new();
            std::fs::File::open(entry.path())
                .ok()?
                .take(65536)
                .read_to_string(&mut text)
                .ok()?;
            if let Ok(connection) = toml::from_str::<Connection>(&text) {
                active.push(connection);
            }
        }
        // Concurrent background sessions do not identify the foreground host.
        if active.len() == 1 {
            active.pop()
        } else {
            None
        }
    }
}

impl Drop for PaneSession {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Registration {
    path: PathBuf,
    reports_transport: bool,
}

impl Registration {
    fn new(connection: &Connection) -> Option<Self> {
        let directory = PathBuf::from(std::env::var_os("RIO_SSH_CONTEXT")?);
        if !directory.is_dir() {
            return None;
        }
        let path = directory.join(std::process::id().to_string());
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&path).ok()?;
        let registration = Self {
            path,
            reports_transport: std::io::stdout().is_terminal(),
        };
        file.write_all(toml::to_string(connection).ok()?.as_bytes())
            .ok()?;
        if registration.reports_transport {
            print!("\x1b]1337;SetUserVar=RIO_APP=c3No\x07");
            let _ = std::io::stdout().flush();
        }
        Some(registration)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        if self.reports_transport {
            print!("\x1b]1337;SetUserVar=RIO_SSH=MA==\x07");
            let _ = std::io::stdout().flush();
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    pid > 0 && unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn options(args: &[&str]) -> Options {
        Options::parse(args.iter().map(OsString::from)).unwrap()
    }

    #[test]
    fn native_arguments_are_preserved_after_wrapper_options() {
        let original = [
            "-A",
            "-p",
            "2222",
            "host",
            "printf '%s' 'a b; $x'",
            "--terminfo=false",
        ];
        let mut args = vec!["--ssh", "/custom/ssh", "--"];
        args.extend(original);
        let options = options(&args);
        let command = options.command("xterm-256color");
        let passed: Vec<_> = command.get_args().collect();
        assert_eq!(command.get_program(), "/custom/ssh");
        assert_eq!(
            &passed[passed.len() - original.len()..],
            original.map(OsStr::new)
        );
        assert!(options.terminfo);
    }

    #[test]
    fn disabled_features_leave_the_environment_and_arguments_alone() {
        let options = options(&["--forward-env=false", "--terminfo=false", "host"]);
        let command = options.command("xterm-256color");
        assert_eq!(command.get_envs().count(), 0);
        assert_eq!(command.get_args().collect::<Vec<_>>(), [OsStr::new("host")]);
    }

    #[test]
    fn setup_never_replays_commands_forwards_or_requests_a_terminal() {
        let options = options(&[
            "-A",
            "-L8080:localhost:80",
            "-oRemoteCommand=deploy",
            "host",
            "rm",
            "file",
        ]);
        let connection = options.connection().unwrap();
        let command = setup_command(&connection);
        let args: Vec<_> = command.get_args().map(|a| a.to_str().unwrap()).collect();
        assert!(!args
            .iter()
            .any(|a| a.contains("deploy") || a.contains("8080") || *a == "rm"));
        assert!(args.contains(&"-oRemoteCommand=none"));
        assert!(args.contains(&"-oRequestTTY=no"));
        assert!(args.contains(&"-oBatchMode=yes"));
        assert_eq!(args.last(), Some(&"host"));
        for args in [
            &["-N", "host"][..],
            &["-f", "host"],
            &["-G", "host"],
            &["-W", "host:22", "jump"],
        ] {
            assert!(super::tests::options(args).connection().is_none());
        }
    }

    #[test]
    fn pane_recipe_requires_a_live_local_registration_and_clears_on_exit() {
        let session = PaneSession::new(usize::MAX).unwrap();
        assert!(session.connection().is_none());
        let connection = Connection::new("host", false).unwrap();
        let path = session.directory.join(std::process::id().to_string());
        std::fs::write(&path, toml::to_string(&connection).unwrap()).unwrap();
        assert_eq!(session.connection(), Some(connection));
        std::fs::remove_file(path).unwrap();
        assert!(session.connection().is_none());
        std::fs::write(session.directory.join("0"), "destination = 'stale'").unwrap();
        assert!(session.connection().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn native_child_receives_terminal_fallback_and_keeps_setenv_and_exit_status() {
        use std::os::unix::fs::PermissionsExt;
        let session = PaneSession::new(usize::MAX - 1).unwrap();
        let script = session.directory.join("ssh");
        std::fs::write(
            &script,
            "#!/bin/sh\nprintf '%s\\n' \"$TERM\" \"$@\"\nexit 23\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let options = options(&[
            "--forward-env=false",
            "--ssh",
            "/bin/sh",
            "--",
            script.to_str().unwrap(),
            "-oSetEnv=EDITOR=vim",
            "host",
            "printf 'a b'",
        ]);
        let output = options.command("xterm-256color").output().unwrap();
        assert_eq!(exit_code(output.status), 23);
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.starts_with("xterm-256color\n"));
        assert!(text.contains("-oSetEnv=EDITOR=vim\n"));
        assert!(text.ends_with("host\nprintf 'a b'\n"));
    }

    #[cfg(unix)]
    #[test]
    fn terminfo_setup_sends_the_embedded_definition_without_replaying_commands() {
        use std::os::unix::fs::PermissionsExt;
        let session = PaneSession::new(usize::MAX - 2).unwrap();
        let program = session.directory.join("ssh");
        let received = session.directory.join("received");
        let script = format!(
            "#!/bin/sh\nfor arg do\nif test \"$arg\" = -G; then\nprintf 'user test\\nhostname fixture.invalid\\nport 22\\n'; exit 0\nfi\ndone\ncat > '{}'\n",
            received.display(),
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
            .unwrap();
        let connection =
            options(&["--ssh", program.to_str().unwrap(), "host", "never-run-this"])
                .connection()
                .unwrap();
        assert!(install_terminfo(&connection, false));
        assert_eq!(std::fs::read_to_string(received).unwrap(), TERMINFO);
    }
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return false;
        }
        let mut code = 0;
        let alive = GetExitCodeProcess(process, &mut code) != 0 && code == 259;
        CloseHandle(process);
        alive
    }
}
