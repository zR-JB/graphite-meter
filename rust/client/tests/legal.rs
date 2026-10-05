use std::process::Command;

#[test]
fn legal_without_embedded_notices_names_the_task_that_embeds_them() {
    for arg in ["-legal", "--legal"] {
        let output = Command::new(env!("CARGO_BIN_EXE_graphite-meter-client"))
            .arg(arg)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
        let expected = "graphite-meter-client: this build embeds no notices; mise run rust-client-run -- --legal \
                        builds the TUI with dependency notices and prints them\n";
        assert_eq!(String::from_utf8(output.stderr).unwrap(), expected);
    }
}
