use std::{collections::HashMap, sync::Mutex};

use crate::*;

// Setting environment variables is not thread-safe on Unix and Linux.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

#[test]
fn test_expand_env_vars() {
    let _m = ENV_MUTEX.lock();
    unsafe {
        env::set_var("MY_VAR1", "hello");
        let result = utils::expand_env_vars("${MY_VAR1} world");
        env::remove_var("MY_VAR1");
        assert_eq!(result, "hello world");
    }
}

#[test]
fn test_expand_env_vars_unset() {
    let _m = ENV_MUTEX.lock();
    unsafe {
        env::remove_var("MY_VAR2");
        let result = utils::expand_env_vars("${MY_VAR2} world");
        assert_eq!(result, " world");
    }
}

#[test]
fn test_expand_env_vars_invalid_no_closing_braket() {
    let result = utils::expand_env_vars("${MY_VAR world");
    assert_eq!(result, "${MY_VAR world");
}

#[test]
fn test_expand_env_vars_invalid_spaces() {
    let result = utils::expand_env_vars("${MY_VAR world}");
    assert_eq!(result, "${MY_VAR world}");
}

#[test]
fn test_expand_env_vars_remove_escape_backslash() {
    let result = utils::expand_env_vars(r"\$MY_VAR world");
    assert_eq!(result, "$MY_VAR world");
}

#[test]
fn test_expand_env_vars_ignore_double_backslash() {
    let _m = ENV_MUTEX.lock();
    unsafe {
        env::set_var("MY_VAR3", "hello");
        let result = utils::expand_env_vars(r"\\$MY_VAR3 world");
        env::remove_var("MY_VAR3");
        assert_eq!(result, r"\hello world");
    }
}

#[test]
fn test_subst_vars() {
    let mut vars = HashMap::new();
    vars.insert("greeting".to_string(), "hello".to_string());

    assert_eq!(
        utils::substitute_global_vars(r"{{ greeting }} world", &vars),
        "hello world"
    );
    assert_eq!(
        utils::substitute_global_vars(r"{{greeting}} world", &vars),
        "hello world"
    );
}

#[test]
fn test_subst_vars_invalid() {
    let mut vars = HashMap::new();
    vars.insert("greeting".to_string(), "hello".to_string());

    assert_eq!(
        utils::substitute_global_vars(r"{{ greet ing }} world", &vars),
        "{{ greet ing }} world"
    );
    assert_eq!(
        utils::substitute_global_vars(r"{{greeting world", &vars),
        "{{greeting world"
    );
}

#[test]
fn test_subst_vars_non_existant() {
    let vars = HashMap::new();

    assert_eq!(
        utils::substitute_global_vars(r"{{ greeting }} world", &vars),
        "{{ greeting }} world"
    );
    assert_eq!(
        utils::substitute_global_vars(r"{{greeting}} world", &vars),
        "{{greeting}} world"
    );
}

#[test]
fn test_subst_vars_strings() {
    let mut vars = HashMap::new();
    vars.insert("greeting".to_string(), "hello".to_string());

    assert_eq!(
        utils::substitute_global_vars(r"{{ '{{' }} greeting {{ '}}' }} world", &vars),
        "{{ greeting }} world"
    );
    assert_eq!(
        utils::substitute_global_vars("{{\"{{\"}} greeting {{\"}}\"}} world", &vars),
        "{{ greeting }} world"
    );
}

#[test]
fn test_tab_complete_unique_command_gets_trailing_space() {
    let services: Vec<String> = vec![];
    assert_eq!(tui::complete("r", &services), Some("restart ".to_string()));
    assert_eq!(tui::complete("star", &services), Some("start ".to_string()));
    assert_eq!(tui::complete("sh", &services), Some("shutdown ".to_string()));
}

#[test]
fn test_tab_complete_common_prefix_or_bell() {
    let services: Vec<String> = vec![];
    // start/stop/status/shutdown share only "s", so no progress -> None (bell):
    assert_eq!(tui::complete("s", &services), None);
    // start/stop/status share "st", no progress beyond what's typed:
    assert_eq!(tui::complete("st", &services), None);
    // empty input completes to the empty common prefix -> None (bell):
    assert_eq!(tui::complete("", &services), None);
    // unknown command word -> None (bell):
    assert_eq!(tui::complete("zz", &services), None);
}

#[test]
fn test_tab_complete_service_names() {
    let services: Vec<String> = vec![
        "hello-world".to_string(),
        "helper".to_string(),
        "health".to_string(),
        "wait-blocking".to_string(),
    ];
    // unique match completes the full name without trailing space:
    assert_eq!(
        tui::complete("start wait", &services),
        Some("start wait-blocking".to_string())
    );
    // multiple matches complete to the longest common prefix:
    assert_eq!(
        tui::complete("start h", &services),
        Some("start he".to_string())
    );
    // tags are not completed:
    assert_eq!(tui::complete("start @", &services), None);
    // commands without a service argument don't complete a second word:
    assert_eq!(tui::complete("status x", &services), None);
    // no matching service -> None (bell):
    assert_eq!(tui::complete("stop zzz", &services), None);
}
