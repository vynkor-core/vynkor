use clap::CommandFactory;
use clap_complete::{generate, Shell};
use veyron::cli::Cli;

fn completion_output(shell: Shell) -> String {
    let mut cmd = Cli::command();
    let mut buf = Vec::new();
    generate(shell, &mut cmd, "vyn", &mut buf);
    String::from_utf8(buf).expect("valid utf8")
}

#[test]
fn completions_zsh_non_empty() {
    let out = completion_output(Shell::Zsh);
    assert!(!out.is_empty(), "zsh completion output must not be empty");
}

#[test]
fn completions_bash_non_empty() {
    let out = completion_output(Shell::Bash);
    assert!(!out.is_empty(), "bash completion output must not be empty");
}

#[test]
fn completions_fish_non_empty() {
    let out = completion_output(Shell::Fish);
    assert!(!out.is_empty(), "fish completion output must not be empty");
}

#[test]
fn completions_zsh_contains_vyn() {
    let out = completion_output(Shell::Zsh);
    assert!(out.contains("vyn"), "zsh script must reference 'vyn'");
}
