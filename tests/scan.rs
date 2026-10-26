//! The coverage checker: the write styles that skip SeaORM's lifecycle hooks.

use std::fs;
use std::path::Path;

use seaorm_auto_audit::scan::{Checker, FORBIDDEN_PATTERNS};
use tempfile::TempDir;

fn scan(source: &str) -> Vec<String> {
    let checker = Checker::new();
    let violations = checker.scan_source(Path::new("src/lib.rs"), source);

    violations
        .into_iter()
        .map(|violation| violation.pattern.to_owned())
        .collect()
}

#[test]
fn it_finds_the_repository_style_writes() {
    let source = r#"
        fn write(db: &DatabaseConnection) {
            users::Entity::insert(am).exec(db).await.unwrap();
            users::Entity::insert_many(models).exec(db).await.unwrap();
            users::Entity::update(am).filter(..).exec(db).await.unwrap();
            users::Entity::update_many().exec(db).await.unwrap();
            users::Entity::delete_many().exec(db).await.unwrap();
            users::Entity::delete_by_id(id).exec(db).await.unwrap();
            am.update_without_returning(db).await.unwrap();
        }
    "#;

    let found = scan(source);

    for pattern in FORBIDDEN_PATTERNS {
        assert!(
            found.contains(&pattern.to_string()),
            "expected `{pattern}` to be found, found: {found:?}"
        );
    }
}

#[test]
fn it_finds_the_writes_of_any_entity() {
    let source = "let x = accounts::Entity::insert(am).exec(db).await;";

    assert_eq!(scan(source), vec!["Entity::insert("]);
}

#[test]
fn it_finds_insert_many() {
    let source = "let x = accounts::Entity::insert_many(models).exec(db).await;";

    assert_eq!(scan(source), vec!["Entity::insert_many"]);
}

#[test]
fn it_allows_the_audited_bulk_wrappers() {
    let source = r#"
        let txn = db.begin().await?;

        seaorm_auto_audit::insert_many::<users::Entity, _, _>(&txn, models).await?;
        seaorm_auto_audit::update_many::<users::Entity, _>(&txn, filter, values).await?;
        seaorm_auto_audit::delete_many::<users::Entity, _>(&txn, filter).await?;
        seaorm_auto_audit::delete_by_id::<users::Entity, _, _>(&txn, id).await?;

        txn.commit().await?;
    "#;

    assert!(
        scan(source).is_empty(),
        "the audited wrappers are the sanctioned bulk writes"
    );
}

#[test]
fn it_ignores_comments() {
    let source = r#"
        // users::Entity::insert(am).exec(db).await;
        /* users::Entity::update(am).exec(db).await; */
        /// users::Entity::delete_many().exec(db).await;
        //! users::Entity::delete_by_id(id).exec(db).await;
    "#;

    assert!(scan(source).is_empty(), "comments are not call sites");
}

#[test]
fn it_ignores_string_literals() {
    let source = r##"
        let hint = "users::Entity::insert(am).exec(db).await";
        let raw = r#"users::Entity::update(am).exec(db).await"#;
    "##;

    assert!(scan(source).is_empty(), "strings are not call sites");
}

#[test]
fn it_ignores_lifetimes_and_character_literals() {
    let source = r#"
        fn borrow<'a>(value: &'a str) -> &'a str {
            let separator = ',';
            value
        }
    "#;

    assert!(scan(source).is_empty());
}

#[test]
fn it_ignores_byte_strings() {
    let source = r#"let blob = b"users::Entity::insert(am).exec(db).await";"#;

    assert!(scan(source).is_empty(), "byte strings are not call sites");
}

#[test]
fn it_ignores_nested_block_comments() {
    // Rust block comments nest: the call is still inside the comment
    // after the first `*/`.
    let source = "/* outer /* users::Entity::insert(am); */ still a comment */";

    assert!(
        scan(source).is_empty(),
        "nested comments are not call sites"
    );
}

#[test]
fn it_ignores_an_escaped_quote_inside_a_string() {
    // The `\"` does not end the string, so the insert is still inside
    // it; the update after the string is a real call site.
    let source =
        r#"let s = "a \" users::Entity::insert(am) b"; users::Entity::update(am).exec(db);"#;

    assert_eq!(scan(source), vec!["Entity::update("]);
}

#[test]
fn it_treats_an_unterminated_string_as_blank_to_the_end() {
    let source = "let s = \"users::Entity::insert(am);";

    assert!(scan(source).is_empty());
}

#[test]
fn it_ignores_an_escaped_quote_character_literal() {
    let source = r"let quote = '\''; users::Entity::insert(am).exec(db);";

    assert_eq!(scan(source), vec!["Entity::insert("]);
}

#[test]
fn the_default_checker_uses_the_builtin_patterns() {
    let checker = Checker::default();
    assert_eq!(checker.forbidden_patterns(), FORBIDDEN_PATTERNS);
    assert!(checker.allowlist().is_empty());

    let mut checker = Checker::default();
    checker.allow("migrations");
    assert_eq!(checker.allowlist().len(), 1);
    assert!(checker.allowlist()[0].ends_with("migrations"));
}

#[test]
fn it_scans_nothing_for_a_non_rust_file() {
    let directory = TempDir::new().expect("a temporary directory");
    let file = directory.path().join("notes.txt");
    fs::write(&file, "users::Entity::insert(am);").expect("the file");

    assert!(Checker::new().scan(&file).is_empty());
}

#[cfg(unix)]
#[test]
fn it_skips_a_file_it_cannot_read() {
    use std::os::unix::fs::PermissionsExt;

    let directory = TempDir::new().expect("a temporary directory");
    let file = directory.path().join("secret.rs");
    fs::write(&file, "users::Entity::insert(am);").expect("the file");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o000)).expect("the permissions");

    assert!(Checker::new().scan(directory.path()).is_empty());
}

#[test]
fn it_reports_the_line_of_the_call_site() {
    let source = "fn main() {\n    let x = 1;\n    users::Entity::insert(am).exec(db);\n}";

    let checker = Checker::new();
    let violations = checker.scan_source(Path::new("src/users.rs"), source);

    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].line, 3);
    assert_eq!(violations[0].pattern, "Entity::insert(");
    assert!(violations[0].text.contains("users::Entity::insert"));
}

#[test]
fn it_reports_a_readable_message() {
    let checker = Checker::new();
    let violations = checker.scan_source(
        Path::new("src/users.rs"),
        "users::Entity::insert(am).exec(db);",
    );

    let message = violations[0].to_string();
    assert!(message.contains("src/users.rs:1"));
    assert!(message.contains("Entity::insert("));
    assert!(message.contains("skips the audit hooks"));
}

#[test]
fn it_scans_a_directory_recursively() {
    let directory = TempDir::new().expect("a temporary directory");
    let nested = directory.path().join("src/entities");
    fs::create_dir_all(&nested).expect("the nested directory");
    fs::write(
        nested.join("users.rs"),
        "users::Entity::insert(am).exec(db);",
    )
    .expect("the source file");
    fs::write(directory.path().join("main.rs"), "fn main() {}").expect("the source file");

    let violations = Checker::new().scan(directory.path());

    assert_eq!(violations.len(), 1);
    assert!(violations[0].path.ends_with("users.rs"));
}

#[test]
fn it_skips_target_and_hidden_directories() {
    let directory = TempDir::new().expect("a temporary directory");
    let target = directory.path().join("target");
    fs::create_dir_all(&target).expect("the target directory");
    fs::write(target.join("generated.rs"), "users::Entity::insert(am);").expect("the file");

    let hidden = directory.path().join(".git");
    fs::create_dir_all(&hidden).expect("the hidden directory");
    fs::write(hidden.join("hook.rs"), "users::Entity::insert(am);").expect("the file");

    assert!(Checker::new().scan(directory.path()).is_empty());
}

#[cfg(unix)]
#[test]
fn it_does_not_follow_symlinks() {
    let directory = TempDir::new().expect("a temporary directory");
    fs::write(directory.path().join("main.rs"), "fn main() {}").expect("the source file");
    // A link back at the directory itself. Following it would recurse
    // forever, so this test completing at all is half the assertion.
    std::os::unix::fs::symlink(directory.path(), directory.path().join("again"))
        .expect("the symlink");

    assert!(Checker::new().scan(directory.path()).is_empty());
}

#[test]
fn it_exempts_a_directory() {
    let directory = TempDir::new().expect("a temporary directory");
    let allowed = directory.path().join("audit");
    fs::create_dir_all(&allowed).expect("the allowed directory");
    fs::write(allowed.join("helper.rs"), "users::Entity::insert(am);").expect("the file");

    let mut checker = Checker::new();
    checker.allow(allowed.clone());

    assert!(checker.scan(directory.path()).is_empty());
}

#[test]
fn it_exempts_a_file() {
    let directory = TempDir::new().expect("a temporary directory");
    let file = directory.path().join("helper.rs");
    fs::write(&file, "users::Entity::insert(am);").expect("the file");

    let mut checker = Checker::new();
    checker.allow(&file);

    assert!(checker.scan(directory.path()).is_empty());
}

#[test]
fn it_exempts_a_bare_file_name() {
    let directory = TempDir::new().expect("a temporary directory");
    fs::create_dir_all(directory.path().join("a")).expect("a directory");
    fs::create_dir_all(directory.path().join("b")).expect("a directory");
    fs::write(
        directory.path().join("a/audit.rs"),
        "users::Entity::insert(am);",
    )
    .expect("the file");
    fs::write(
        directory.path().join("b/audit.rs"),
        "users::Entity::insert(am);",
    )
    .expect("the file");
    fs::write(
        directory.path().join("b/users.rs"),
        "users::Entity::update(am);",
    )
    .expect("the file");

    let mut checker = Checker::new();
    checker.allow("audit.rs");

    let violations = checker.scan(directory.path());

    assert_eq!(violations.len(), 1, "only the file that is not exempt");
    assert!(violations[0].path.ends_with("users.rs"));
}

#[test]
fn it_can_use_other_patterns() {
    let mut checker = Checker::new();
    checker.patterns(&["execute_unprepared"]);

    let violations = checker.scan_source(
        Path::new("src/users.rs"),
        "db.execute_unprepared(\"DELETE FROM users\").await;",
    );

    assert_eq!(violations.len(), 1);
    assert_eq!(violations[0].pattern, "execute_unprepared");
}

#[test]
fn the_binary_fails_the_build_on_a_forbidden_write() {
    let directory = TempDir::new().expect("a temporary directory");
    fs::write(
        directory.path().join("users.rs"),
        "users::Entity::insert(am).exec(db);",
    )
    .expect("the source file");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .arg(directory.path())
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Entity::insert("), "{stderr}");
}

#[test]
fn the_binary_passes_on_a_clean_tree() {
    let directory = TempDir::new().expect("a temporary directory");
    fs::write(directory.path().join("users.rs"), "am.insert(db).await;").expect("the source file");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .arg(directory.path())
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn the_binary_needs_something_to_scan() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("usage:"), "{stderr}");
}

#[test]
fn the_binary_prints_usage_for_help() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .arg("--help")
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("usage:"), "{stdout}");
}

#[test]
fn the_binary_rejects_an_unknown_option() {
    let directory = TempDir::new().expect("a temporary directory");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .arg("--bogus")
        .arg(directory.path())
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("unknown option `--bogus`"), "{stderr}");
}

#[test]
fn the_binary_rejects_allow_without_a_path() {
    let directory = TempDir::new().expect("a temporary directory");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .arg(directory.path())
        .arg("--allow")
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--allow needs a path"), "{stderr}");
}

#[test]
fn the_binary_rejects_a_path_that_does_not_exist() {
    let directory = TempDir::new().expect("a temporary directory");
    let missing = directory.path().join("no_such_dir");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .arg(&missing)
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("does not exist"), "{stderr}");
}

#[test]
fn the_binary_allow_exempts_a_path() {
    let directory = TempDir::new().expect("a temporary directory");
    let file = directory.path().join("audit.rs");
    fs::write(&file, "users::Entity::insert(am).exec(db);").expect("the source file");

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_seaorm-auto-audit-check"))
        .arg(directory.path())
        .arg("--allow")
        .arg(&file)
        .output()
        .expect("the checker should run");

    assert_eq!(output.status.code(), Some(0), "the only write is exempt");
}
