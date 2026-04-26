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
