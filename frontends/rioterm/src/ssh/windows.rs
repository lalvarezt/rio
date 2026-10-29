//! Native Windows OpenSSH. Each pane owns an independent SSH process.
use super::*;

pub(super) fn program() -> PathBuf {
    if let Some(root) = std::env::var_os("SystemRoot") {
        let path = PathBuf::from(root).join("System32/OpenSSH/ssh.exe");
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("ssh.exe")
}

pub(super) fn shell(
    connection: &Connection,
    directory: Option<&Path>,
    duplicate: bool,
) -> Result<Shell, String> {
    // Let the native client own keys, agents, host trust and authentication in
    // the pane's ConPTY. Windows OpenSSH has no ControlMaster support.
    let mut args = [
        "-oControlMaster=no",
        "-oControlPath=none",
        "-oControlPersist=no",
        "-oForkAfterAuthentication=no",
        "-oStdinNull=no",
        "-oSessionType=default",
    ]
    .map(String::from)
    .to_vec();
    if duplicate {
        args.extend([
            "-oClearAllForwardings=yes".into(),
            "-oPermitLocalCommand=no".into(),
            "-oRemoteCommand=none".into(),
        ]);
    }
    args.extend(connection.options.clone());
    let mut shell = connection.finish_shell(args, directory)?;
    shell.program = Some(program().to_string_lossy().into_owned());
    Ok(shell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn siblings_use_native_ssh_and_agent_without_replaying_forwards() {
        let connection = Connection::from_args(&[
            "-A".into(),
            "-p".into(),
            "2222".into(),
            "-L".into(),
            "8080:localhost:80".into(),
            "host".into(),
        ])
        .unwrap();
        let shell = shell(&connection, None, true).unwrap();
        assert_eq!(shell.program.as_deref(), program().to_str());
        assert!(shell.args.iter().any(|arg| arg == "-A"));
        assert!(shell.args.windows(2).any(|args| args == ["-p", "2222"]));
        assert!(shell
            .args
            .iter()
            .any(|arg| arg == "-oClearAllForwardings=yes"));
        assert!(shell.args.iter().any(|arg| arg == "-oControlPath=none"));
        assert!(!shell
            .args
            .iter()
            .any(|arg| arg == "-oPreferredAuthentications=none"));
        assert!(!shell.args.iter().any(|arg| arg.contains("8080")));
    }
}
