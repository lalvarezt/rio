//! Native Windows OpenSSH. Each pane owns an independent SSH process.
use super::*;

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
    connection.finish_shell(args, directory)
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
        assert_eq!(shell.program.as_deref(), Some(connection.program()));
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
