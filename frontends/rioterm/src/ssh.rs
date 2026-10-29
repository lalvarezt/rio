//! SSH launch recipes. Windows panes use independent native OpenSSH processes;
//! other platforms reuse an authenticated transport. Recipes omit commands.

use rio_backend::config::Shell;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[cfg(windows)]
#[path = "ssh/windows.rs"]
mod windows;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub destination: String,
    #[serde(default)]
    pub powershell: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    remote_shell: Option<String>,
    #[serde(default)]
    options: Vec<String>,
}

impl Connection {
    pub fn new(destination: &str, powershell: bool) -> Result<Self, String> {
        let destination = destination.trim();
        if destination.is_empty()
            || destination.starts_with('-')
            || destination
                .chars()
                .any(|c| c.is_whitespace() || c.is_control())
        {
            return Err("Enter an SSH host alias, user@host, or ssh:// address.".into());
        }
        Ok(Self {
            destination: destination.into(),
            powershell,
            remote_shell: None,
            options: Vec::new(),
        })
    }

    /// Accept SSH's argv, not a shell command string. Options with side effects
    /// (forwards, control commands, backgrounding) must not run in sibling panes.
    pub fn from_shell(shell: &Shell) -> Option<Self> {
        let program = shell.program.as_deref()?.rsplit(['/', '\\']).next()?;
        if !program.eq_ignore_ascii_case("ssh")
            && !program.eq_ignore_ascii_case("ssh.exe")
        {
            return None;
        }
        Self::from_args(&shell.args)
    }

    fn from_args(args: &[String]) -> Option<Self> {
        let mut options = Vec::new();
        let mut index = 0;
        let mut destination = None;
        while let Some(arg) = args.get(index) {
            if arg == "--" {
                index += 1;
                if destination.is_none() {
                    destination = args.get(index).map(String::as_str);
                    index += 1;
                }
                break;
            }
            if !arg.starts_with('-') || arg == "-" {
                if destination.is_some() {
                    break;
                }
                destination = Some(arg.as_str());
                index += 1;
                continue;
            }
            for (offset, flag) in arg[1..].char_indices() {
                match flag {
                    // These are not interactive SSH connections.
                    'O' | 'W' | 'G' | 'V' | 'Q' | 's' => return None,
                    'B' | 'b' | 'c' | 'D' | 'E' | 'e' | 'F' | 'I' | 'i' | 'J' | 'L'
                    | 'l' | 'm' | 'o' | 'P' | 'p' | 'R' | 'S' | 'w' => {
                        let value_start = offset + 2;
                        let value = if value_start < arg.len() {
                            arg[value_start..].to_string()
                        } else {
                            index += 1;
                            args.get(index)?.clone()
                        };
                        if !(matches!(flag, 'D' | 'E' | 'L' | 'R' | 'w')
                            || flag == 'o' && unsafe_duplicate_option(&value))
                        {
                            options.extend([format!("-{flag}"), value]);
                        }
                        break;
                    }
                    '4' | '6' | 'A' | 'a' | 'C' | 'K' | 'k' | 'q' | 'v' | 'X' | 'x'
                    | 'Y' | 'y' => {
                        options.push(format!("-{flag}"));
                    }
                    'f' | 'g' | 'M' | 'N' | 'n' | 'T' | 't' => {}
                    _ => return None,
                }
            }
            index += 1;
        }
        let mut connection = Self::new(destination?, false).ok()?;
        connection.options = options;
        // Rio's Windows launcher runs PowerShell explicitly even when the SSH
        // server defaults to cmd.exe. Keep that choice in subsequent panes.
        connection.powershell = args.get(index).is_some_and(|arg| {
            arg.eq_ignore_ascii_case("powershell") || arg.eq_ignore_ascii_case("pwsh")
        });
        // Preserve an explicitly selected interactive shell, never an arbitrary
        // remote command. This includes `ssh host /path/to/fish` workflows.
        if args.len() == index + 1 {
            if let Some(shell) = args.get(index) {
                if matches!(
                    shell.rsplit('/').next(),
                    Some("fish" | "bash" | "zsh" | "sh" | "ksh" | "dash")
                ) {
                    connection.remote_shell = Some(shell.clone());
                }
            }
        }
        Some(connection)
    }

    fn command(&self) -> Command {
        #[cfg(windows)]
        let mut command = Command::new(windows::program());
        #[cfg(not(windows))]
        let mut command = Command::new("ssh");
        command.args(&self.options);
        command
    }

    fn settings(&self) -> Result<String, String> {
        let mut command = self.command();
        command.args(["-G", &self.destination]);
        let output = local_output(command)?;
        if !output.status.success() {
            return Err(command_error("Could not read SSH configuration", &output));
        }
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Windows uses native SSH. Unix connections reuse a ControlMaster socket.
    pub fn connect_shell(&self) -> Result<Shell, String> {
        #[cfg(windows)]
        {
            self.settings()?;
            return windows::shell(self, None, false);
        }
        #[cfg(not(windows))]
        {
            let settings = self.settings()?;
            let path = control_path(&settings)?;
            let mut args = vec![
                "-oControlMaster=auto".into(),
                "-oControlPersist=600".into(),
                "-S".into(),
                path,
            ];
            args.extend(self.options.clone());
            self.finish_shell(args, None)
        }
    }

    /// Siblings preserve the destination and directory, without replaying commands.
    pub fn duplicate_shell(
        &self,
        directory: Option<&Path>,
        reported_host: Option<&str>,
    ) -> Result<Shell, String> {
        let settings = self.settings()?;
        let directory = directory.filter(|_| {
            reported_host.is_none_or(|reported| {
                setting(&settings, "hostname")
                    .is_some_and(|host| same_host(host, reported))
            })
        });
        #[cfg(windows)]
        return windows::shell(self, directory, true);
        #[cfg(not(windows))]
        {
            let mut args = {
                let path = control_path(&settings)?;
                let mut check = self.command();
                check.args(["-S", &path, "-O", "check", &self.destination]);
                let output = local_output(check)?;
                if !output.status.success() {
                    return Err(command_error("The shared SSH connection is unavailable. Reconnect with Connect via SSH before opening another tab or split", &output));
                }
                vec![
                    "-S".into(),
                    path,
                    "-oControlMaster=no".into(),
                    // If the master dies after the check, do not authenticate again.
                    "-oPreferredAuthentications=none".into(),
                ]
            };
            args.extend(
                [
                    "-oBatchMode=yes",
                    "-oConnectTimeout=5",
                    "-oConnectionAttempts=1",
                    "-oClearAllForwardings=yes",
                    "-oRemoteCommand=none",
                    "-oSessionType=default",
                    "-oForkAfterAuthentication=no",
                    "-oStdinNull=no",
                    "-oPermitLocalCommand=no",
                ]
                .map(String::from),
            );
            args.extend(self.options.clone());
            self.finish_shell(args, directory)
        }
    }

    fn finish_shell(
        &self,
        mut args: Vec<String>,
        directory: Option<&Path>,
    ) -> Result<Shell, String> {
        args.extend(["-t".into(), self.destination.clone()]);
        let directory = directory.and_then(Path::to_str).map(remote_path);
        if self.powershell || directory.is_some_and(is_windows_path) {
            let mut script = String::new();
            if let Some(directory) = directory {
                script.push_str(&format!(
                    "Set-Location -LiteralPath '{}' -ErrorAction Stop;\n",
                    directory.replace('\'', "''")
                ));
            }
            // Windows OpenSSH commonly launches through cmd.exe (8191-char
            // command limit). Do not spend that budget on source comments.
            let integration = include_str!("shell_integration/rio.ps1")
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n")
                .replace(
                    "kitty-shell-cwd://$path",
                    "kitty-shell-cwd://$env:COMPUTERNAME$path",
                );
            script.push_str(&integration);
            use base64::Engine as _;
            let bytes: Vec<u8> =
                script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
            if encoded.len() > 7500 {
                return Err("The remote directory is too long for this Windows SSH command. Open a connection in its home directory instead.".into());
            }
            args.extend([
                "powershell".into(),
                "-NoLogo".into(),
                "-NoExit".into(),
                "-EncodedCommand".into(),
                encoded,
            ]);
        } else if let Some(directory) = directory {
            let shell = self
                .remote_shell
                .as_deref()
                .map(posix_quote)
                .unwrap_or_else(|| "\"${SHELL:-/bin/sh}\"".into());
            let script = format!("cd {} && exec {shell} -l", posix_quote(directory));
            args.push(format!("sh -c {}", posix_quote(&script)));
        } else if let Some(shell) = &self.remote_shell {
            args.push(posix_quote(shell));
        }
        Ok(Shell {
            program: Some("ssh".into()),
            args,
        })
    }
}

fn unsafe_duplicate_option(value: &str) -> bool {
    let key = value.split(['=', ' ', '\t']).next().unwrap_or("");
    [
        "LocalForward",
        "RemoteForward",
        "DynamicForward",
        "RemoteCommand",
        "SessionType",
        "ForkAfterAuthentication",
        "StdinNull",
        "ControlMaster",
        "ControlPersist",
        "BatchMode",
        "PreferredAuthentications",
        "ClearAllForwardings",
    ]
    .iter()
    .any(|name| key.eq_ignore_ascii_case(name))
}

fn setting<'a>(settings: &'a str, key: &str) -> Option<&'a str> {
    settings
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))
}

fn command_error(context: &str, output: &std::process::Output) -> String {
    let diagnostic = String::from_utf8_lossy(&output.stderr);
    let diagnostic = diagnostic.trim();
    if diagnostic.is_empty() {
        format!("{context} ({}).", output.status)
    } else {
        format!("{context}:\n{diagnostic}")
    }
}

fn same_host(expected: &str, reported: &str) -> bool {
    let expected = expected.trim_end_matches('.');
    let reported = reported.trim_end_matches('.');
    expected.eq_ignore_ascii_case(reported)
        || ((!expected.contains('.') || !reported.contains('.'))
            && expected
                .split('.')
                .next()
                .unwrap_or("")
                .eq_ignore_ascii_case(reported.split('.').next().unwrap_or("")))
}

fn remote_path(path: &str) -> &str {
    path.strip_prefix('/')
        .filter(|p| is_windows_path(p))
        .unwrap_or(path)
}

fn is_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn posix_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(not(windows))]
fn control_path(settings: &str) -> Result<String, String> {
    setting(settings, "controlpath")
        .filter(|path| *path != "none")
        .map(str::to_owned)
        .map(Ok)
        .unwrap_or_else(default_control_path)
}

#[cfg(not(windows))]
fn default_control_path() -> Result<String, String> {
    let dir = dirs::cache_dir()
        .ok_or("Could not locate the SSH socket directory.")?
        .join("rio/ssh");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create SSH socket directory: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| format!("Could not protect SSH socket directory: {e}"))?;
    }
    Ok(dir.join("%C").to_string_lossy().into_owned())
}

/// Bound configuration and sharing checks. Match exec in a user's SSH config
/// must not leave the window waiting indefinitely.
fn local_output(mut command: Command) -> Result<std::process::Output, String> {
    use std::io::Read;
    let program = command.get_program().to_string_lossy().into_owned();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Preflight checks have no terminal and must not flash a console window.
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start {program}: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let stderr = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let stdout = stdout
                    .join()
                    .map_err(|_| "SSH output reader failed.")?
                    .map_err(|e| e.to_string())?;
                let stderr = stderr
                    .join()
                    .map_err(|_| "SSH error reader failed.")?
                    .map_err(|e| e.to_string())?;
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) if start.elapsed() < Duration::from_secs(2) => {
                std::thread::sleep(Duration::from_millis(5))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("SSH configuration or connection check timed out.".into());
            }
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct History {
    #[serde(default)]
    connections: Vec<Connection>,
}

fn history_path() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("rio/ssh-connections.toml"))
}

/// Store connection recipes only, without credentials.
pub fn remember(connection: &Connection) -> Result<(), String> {
    let path = history_path().ok_or("Could not locate the connection history.")?;
    let mut history = read_history();
    let mut connection = connection.clone();
    connection.options = saved_options(&connection.options);
    history.connections.retain(|item| {
        item.destination != connection.destination
            || item.powershell != connection.powershell
    });
    history.connections.insert(0, connection);
    history.connections.truncate(32);
    let text = toml::to_string(&history).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| e.to_string())
}

fn saved_options(original: &[String]) -> Vec<String> {
    // Arbitrary -o values may contain proxy credentials or SetEnv secrets.
    // Save identity and references to SSH-owned configuration only.
    let mut options = Vec::new();
    let mut index = 0;
    while index < original.len() {
        let option = &original[index];
        if option.len() == 2 && "BbcDEeFIiJLlmopPRSw".contains(&option[1..]) {
            if let Some(value) = original.get(index + 1) {
                let key = value.split(['=', ' ', '\t']).next().unwrap_or("");
                let identity_option = option == "-o"
                    && [
                        "HostName",
                        "User",
                        "Port",
                        "IdentityFile",
                        "CertificateFile",
                        "IdentitiesOnly",
                        "IdentityAgent",
                        "ProxyJump",
                        "HostKeyAlias",
                    ]
                    .iter()
                    .any(|allowed| key.eq_ignore_ascii_case(allowed));
                if identity_option
                    || matches!(option.as_str(), "-p" | "-l" | "-i" | "-F" | "-J" | "-P")
                {
                    options.extend([option.clone(), value.clone()]);
                }
            }
            index += 1;
        } else if matches!(option.as_str(), "-4" | "-6") {
            options.push(option.clone());
        }
        index += 1;
    }
    options
}

fn read_history() -> History {
    history_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn connections(powershell: bool) -> Vec<Connection> {
    let mut connections = read_history().connections;
    connections.retain(|connection| connection.powershell == powershell);
    if let Some(path) = dirs::home_dir().map(|p| p.join(".ssh/config")) {
        if let Ok(config) = std::fs::read_to_string(path) {
            for line in config.lines() {
                let mut words = line.split_whitespace();
                if words
                    .next()
                    .is_some_and(|word| word.eq_ignore_ascii_case("Host"))
                {
                    for alias in words.take_while(|word| !word.starts_with('#')) {
                        if !alias.contains(['*', '?', '!']) {
                            if let Ok(connection) = Connection::new(alias, powershell) {
                                if !connections.contains(&connection) {
                                    connections.push(connection);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    connections
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Option<Connection> {
        Connection::from_args(&args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn extracts_identity_and_options_without_replaying_commands_or_forwards() {
        let connection = parse(&[
            "-vv",
            "-p2222",
            "-J",
            "jump",
            "-L8080:localhost:80",
            "-o",
            "RemoteCommand=deploy",
            "user@host",
            "rm",
            "file",
        ])
        .unwrap();
        assert_eq!(connection.destination, "user@host");
        assert_eq!(connection.options, ["-v", "-v", "-p", "2222", "-J", "jump"]);
        assert!(!connection.powershell);
        assert!(parse(&["-O", "exit", "host"]).is_none());
        assert!(parse(&["-p"]).is_none());
    }

    #[test]
    fn destination_is_an_argument_never_a_shell_command() {
        for invalid in [
            "",
            "-oProxyCommand=evil",
            "host\ncommand",
            "user@host command",
        ] {
            assert!(Connection::new(invalid, false).is_err());
        }
        assert!(Connection::new("ssh://user@[::1]:2222", false).is_ok());
    }

    #[test]
    fn preserves_identity_options_after_destination_and_in_saved_recipes() {
        let connection = parse(&[
            "host",
            "-p2222",
            "-oUser=alice",
            "-J",
            "jump",
            "-o",
            "SetEnv=TOKEN=secret",
            "-oProxyCommand=helper secret",
            "powershell",
        ])
        .unwrap();
        assert!(connection.powershell);
        assert_eq!(
            saved_options(&connection.options),
            ["-p", "2222", "-o", "User=alice", "-J", "jump"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn configuration_failure_preserves_openssh_diagnostic() {
        let missing = std::env::temp_dir()
            .join(format!("rio-missing-ssh-config-{}", std::process::id()));
        let connection =
            parse(&["-F", missing.to_str().unwrap(), "no-network.invalid"]).unwrap();
        let error = connection.settings().unwrap_err();
        assert!(error.contains(missing.to_str().unwrap()), "{error}");
        assert!(error.contains("No such file"), "{error}");
    }

    #[cfg(windows)]
    #[test]
    fn windows_delegates_host_trust_and_authentication_to_native_ssh() {
        let connection = parse(&[
            "-F",
            "NUL",
            "-oUserKnownHostsFile=NUL",
            "-oGlobalKnownHostsFile=NUL",
            "example.invalid",
        ])
        .unwrap();
        let shell = connection.connect_shell().unwrap();
        assert_eq!(shell.program.as_deref(), windows::program().to_str());
        assert!(!shell
            .args
            .iter()
            .any(|arg| arg.contains("StrictHostKeyChecking=no")));
        assert!(!shell.args.iter().any(|arg| arg.contains("BatchMode=yes")));
    }

    #[cfg(windows)]
    #[test]
    fn windows_sibling_needs_no_shared_master() {
        let connection = parse(&["-F", "NUL", "user@example.invalid"]).unwrap();
        let shell = connection
            .duplicate_shell(Some(Path::new("/tmp/a'b")), Some("example.invalid"))
            .unwrap();
        assert!(shell.args.iter().any(|arg| arg == "-oControlPath=none"));
        assert!(shell.args.last().unwrap().contains("exec"));
    }

    #[test]
    fn selected_fish_survives_siblings_without_replaying_arbitrary_commands() {
        let connection =
            parse(&["-A", "host", "-t", "/home/linuxbrew/.linuxbrew/bin/fish"]).unwrap();
        assert_eq!(
            connection.remote_shell.as_deref(),
            Some("/home/linuxbrew/.linuxbrew/bin/fish")
        );
        let shell = connection
            .finish_shell(vec![], Some(Path::new("/tmp/a b")))
            .unwrap();
        assert!(shell.args.last().unwrap().contains("fish"));
        assert!(shell.args.last().unwrap().contains("exec"));
        for command in [
            vec!["host", "rm", "file"],
            vec!["host", "fish", "-c", "deploy"],
            vec!["host", "fish;deploy"],
        ] {
            assert!(parse(&command).unwrap().remote_shell.is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn missing_control_socket_fails_without_connecting_or_authenticating() {
        let connection = parse(&[
            "-F",
            "/dev/null",
            "-S",
            "/nonexistent-rio-test/control",
            "no-network.invalid",
        ])
        .unwrap();
        assert!(connection
            .duplicate_shell(None, None)
            .unwrap_err()
            .contains("unavailable"));
    }

    #[cfg(unix)]
    #[test]
    fn directory_survives_both_remote_shell_parsing_layers() {
        let connection = Connection::new("host", false).unwrap();
        let shell = connection
            .finish_shell(Vec::new(), Some(Path::new("/tmp/a'b; $(touch nope)")))
            .unwrap();
        let remote = shell.args.last().unwrap();
        let output = Command::new("sh")
            .args([
                "-c",
                &format!("printf '%s' {}", remote.strip_prefix("sh -c ").unwrap()),
            ])
            .output()
            .unwrap();
        let inner = String::from_utf8(output.stdout).unwrap();
        assert!(inner.starts_with("cd '/tmp/a'\\''b; $(touch nope)' && exec"));
    }

    #[test]
    fn windows_cwd_is_encoded_for_powershell_even_on_a_unix_client() {
        use base64::Engine as _;
        let connection = Connection::new("alice@windows.example", false).unwrap();
        let shell = connection
            .finish_shell(Vec::new(), Some(Path::new("/C:\\Users\\a'b $x")))
            .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(shell.args.last().unwrap())
            .unwrap();
        let utf16: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let script = String::from_utf16(&utf16).unwrap();
        assert!(shell.args.last().unwrap().len() < 6000);
        assert!(script.starts_with(
            "Set-Location -LiteralPath 'C:\\Users\\a''b $x' -ErrorAction Stop;"
        ));
        assert!(script.contains("function Global:Prompt"));
    }
}
