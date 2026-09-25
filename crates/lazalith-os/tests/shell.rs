use lazalith_os::{
    HeadlessShell, SHELL_PROMPT, ShellCommand, ShellError, ShellOutcome, VirtualFileSystem,
};

fn shell() -> HeadlessShell {
    let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
    filesystem.insert_directory(b"/dir").unwrap();
    filesystem.insert_file(b"/dir/z.txt", b"z").unwrap();
    filesystem.insert_file(b"/dir/a.txt", b"a").unwrap();
    filesystem.insert_file(b"/hello.txt", b"hello").unwrap();
    filesystem.insert_file(b"/init.lzx", b"not-an-lzx").unwrap();
    HeadlessShell::new(filesystem)
}

#[test]
fn headless_shell_renders_prompt_and_required_filesystem_commands() {
    let mut shell = shell();
    shell.print_prompt().unwrap();
    assert_eq!(
        shell.execute_line(b"help").unwrap().command,
        ShellCommand::Help
    );
    assert_eq!(
        shell.execute_line(b"echo hello world").unwrap().command,
        ShellCommand::Echo
    );
    assert_eq!(
        shell.execute_line(b"ls /dir").unwrap().command,
        ShellCommand::Ls
    );
    assert_eq!(
        shell.execute_line(b"cat /hello.txt").unwrap().command,
        ShellCommand::Cat
    );

    let output = shell.output();
    assert_eq!(
        output,
        b"lazos$ commands: help echo ls cat run clear\nhello world\na.txt\nz.txt\nhello"
    );
}

#[test]
fn clear_resets_transcript_and_advances_generation() {
    let mut shell = shell();
    shell.print_prompt().unwrap();
    shell.execute_line(b"echo before").unwrap();
    let outcome = shell.execute_line(b"clear").unwrap();
    assert!(outcome.cleared);
    assert_eq!(shell.output(), b"");
    assert_eq!(shell.screen_generation(), 1);
    shell.print_prompt().unwrap();
    assert_eq!(shell.output(), SHELL_PROMPT);
}

#[test]
fn shell_rejects_invalid_commands_without_transcript_mutation() {
    let mut shell = shell();
    let initial = shell.output().to_vec();
    assert!(matches!(
        shell.execute_line(b"unknown"),
        Err(ShellError::UnknownCommand { .. })
    ));
    let missing = shell.execute_line(b"cat");
    assert!(matches!(
        missing,
        Err(ShellError::MissingArgument {
            command: ShellCommand::Cat
        })
    ));
    assert!(
        ShellError::MissingArgument {
            command: ShellCommand::Cat
        }
        .to_string()
        .contains("cat")
    );
    assert!(matches!(
        shell.execute_line(b"ls /dir extra"),
        Err(ShellError::ExtraArguments {
            command: ShellCommand::Ls
        })
    ));
    assert!(matches!(
        shell.execute_line(b"run /init.lzx"),
        Ok(ShellOutcome {
            command: ShellCommand::Run,
            ..
        })
    ));
    assert!(matches!(
        shell.take_pending_image(),
        Err(ShellError::Image(_))
    ));
    assert!(matches!(
        shell.execute_line(b"run /init.lzx"),
        Err(ShellError::PendingRun)
    ));
    assert_eq!(shell.output(), initial);
}

#[test]
fn shell_enforces_line_and_output_limits() {
    let mut filesystem = VirtualFileSystem::with_defaults().unwrap();
    filesystem.insert_file(b"/big", &[0; 4096]).unwrap();
    let mut shell = HeadlessShell::with_limits(filesystem, 8, 20).unwrap();
    assert!(matches!(
        shell.execute_line(b"cat /big"),
        Err(ShellError::OutputLimit { maximum: 8 })
    ));
    assert_eq!(shell.output(), b"");
    assert!(matches!(
        shell.execute_line(b"help extra"),
        Err(ShellError::ExtraArguments {
            command: ShellCommand::Help
        })
    ));
    let mut line_limited =
        HeadlessShell::with_limits(VirtualFileSystem::with_defaults().unwrap(), 8, 4).unwrap();
    assert!(matches!(
        line_limited.execute_line(b"help extra"),
        Err(ShellError::LineTooLong {
            length: 10,
            maximum: 4
        })
    ));
    assert!(matches!(
        HeadlessShell::with_limits(VirtualFileSystem::with_defaults().unwrap(), 0, 1),
        Err(ShellError::InvalidLimits)
    ));
}
