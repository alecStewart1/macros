//! The [`EditorCommand`] payloads the registered primitives build from their
//! Scheme arguments: the boxed argument structs, Helm sources and window
//! layouts.

use super::*;

fn commands(engine: &ScriptEngine, src: &str) -> Vec<EditorCommand> {
    engine
        .eval(src)
        .unwrap_or_else(|e| panic!("{src}: {e}"))
        .commands
}

fn value(engine: &ScriptEngine, src: &str) -> String {
    engine
        .eval(src)
        .unwrap_or_else(|e| panic!("{src}: {e}"))
        .value
        .unwrap_or_default()
}

#[test]
fn grammar_install_carries_every_field_verbatim() {
    let (engine, _rx) = ScriptEngine::new();
    let cmds = commands(
        &engine,
        r#"(grammar-install "zig" "https://example.com/tree-sitter-zig" "v1.0" "zig zon" "" "")"#,
    );
    match cmds.as_slice() {
        [EditorCommand::InstallGrammar(g)] => {
            assert_eq!(g.id, "zig");
            assert_eq!(g.repo, "https://example.com/tree-sitter-zig");
            assert_eq!(g.rev, "v1.0");
            assert_eq!(g.extensions, "zig zon");
            // "" means the default symbol and the repo root.
            assert_eq!((g.symbol.as_str(), g.subdir.as_str()), ("", ""));
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn http_requests_pair_up_flat_headers() {
    let (engine, _rx) = ScriptEngine::new();
    for (name, full) in [("http-request", false), ("http-request-full", true)] {
        let src = format!(
            r#"({name} "PATCH" "https://example.com/x" (list "a" "1" "b" "2" "dangling") "{{}}" 'on-reply)"#
        );
        let out = engine.eval(&src).unwrap();
        assert_eq!(out.value.as_deref(), Some("#true"));
        match out.commands.as_slice() {
            [EditorCommand::HttpRequest(r)] => {
                assert_eq!(r.method, "PATCH");
                assert_eq!(r.url, "https://example.com/x");
                // A key without a value is dropped.
                assert_eq!(
                    r.headers,
                    [("a".into(), "1".into()), ("b".into(), "2".into())]
                );
                assert_eq!(r.body, "{}");
                assert_eq!(r.on_response, "on-reply");
                assert_eq!(r.full, full);
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn http_streams_get_fresh_ids() {
    let (engine, _rx) = ScriptEngine::new();
    let src = r#"(make-http-stream "https://example.com/s" '("k" "v") "body" "on-line" 'on-done)"#;
    let first = engine.eval(src).unwrap();
    let second = engine.eval(src).unwrap();
    let id = |r: &EvalResult| match r.commands.as_slice() {
        [EditorCommand::HttpStream(s)] => {
            assert_eq!(s.headers, [("k".into(), "v".into())]);
            assert_eq!(
                (s.on_line.as_str(), s.on_done.as_str()),
                ("on-line", "on-done")
            );
            assert_eq!(r.value.as_deref(), Some(s.id.to_string().as_str()));
            s.id
        }
        other => panic!("{other:?}"),
    };
    assert_ne!(id(&first), id(&second));
}

#[test]
fn dap_start_treats_an_empty_exit_callback_as_none() {
    let (engine, _rx) = ScriptEngine::new();
    for (on_exit, want) in [(r#""""#, None), ("'on-exit", Some("on-exit"))] {
        let src = format!(r#"(dap-start "lldb-dap" '("--port" "0") "" 'on-msg {on_exit})"#);
        match commands(&engine, &src).as_slice() {
            [EditorCommand::DapStart(d)] => {
                assert_eq!(d.program, "lldb-dap");
                assert_eq!(d.args, ["--port", "0"]);
                assert_eq!(d.cwd, "");
                assert_eq!(d.on_message, "on-msg");
                assert_eq!(d.on_exit.as_deref(), want);
            }
            other => panic!("{other:?}"),
        }
    }
}

#[test]
fn helm_sources_parse_actions_and_previews() {
    let (engine, _rx) = ScriptEngine::new();
    let cmds = commands(
        &engine,
        r#"(helm (list
             (list "Files" '("a.rs" "b.rs") (list (list "Open" "find-file") (list "Delete" "rm")) "preview-file")
             (list "Dynamic" 'list-things 'pick)
             (list "No actions" '("x") '())
             "not a source"))"#,
    );
    let sources = match cmds.as_slice() {
        [EditorCommand::Helm(s)] => s,
        other => panic!("{other:?}"),
    };
    // A source without actions, and one that isn't a list, are dropped.
    assert_eq!(sources.len(), 2);
    let files = &sources[0];
    assert_eq!(files.name, "Files");
    match &files.candidates {
        HelmCandidates::Static(c) => {
            let names: Vec<_> = c.iter().map(|(n, _)| n.as_str()).collect();
            assert_eq!(names, ["a.rs", "b.rs"]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        files.actions,
        [
            ("Open".into(), "find-file".into()),
            ("Delete".into(), "rm".into())
        ]
    );
    assert_eq!(files.preview.as_deref(), Some("preview-file"));
    let dynamic = &sources[1];
    assert!(matches!(&dynamic.candidates, HelmCandidates::Dynamic(f) if f == "list-things"));
    // A single action is the default one.
    assert_eq!(dynamic.actions, [("default".into(), "pick".into())]);
    assert_eq!(dynamic.preview, None);
}

#[test]
fn set_window_layout_parses_trees_and_rejects_malformed_ones() {
    let (engine, _rx) = ScriptEngine::new();
    let out = engine
        .eval(r#"(set-window-layout '(split #t 2.0 (leaf "a.rs" "/p/a.rs" 4 #f) (leaf "*scratch*")))"#)
        .unwrap();
    assert_eq!(out.value.as_deref(), Some("#true"));
    match out.commands.as_slice() {
        [EditorCommand::SetWindowLayout(spec)] => assert_eq!(
            spec,
            &LayoutSpec::Split {
                vertical: true,
                // The ratio is clamped so neither pane vanishes.
                ratio: 0.9,
                a: Box::new(LayoutSpec::Leaf {
                    buffer: "a.rs".into(),
                    path: "/p/a.rs".into(),
                    point: 4,
                    selected: false,
                }),
                // Missing leaf fields take their defaults.
                b: Box::new(LayoutSpec::Leaf {
                    buffer: "*scratch*".into(),
                    path: String::new(),
                    point: 0,
                    selected: false,
                }),
            }
        ),
        other => panic!("{other:?}"),
    }
    for bad in [
        "'(tree)",
        "'(leaf)",
        r#"'(split #f 0.5 (leaf "a"))"#,
        r#"'(split #f "half" (leaf "a") (leaf "b"))"#,
        "42",
    ] {
        let out = engine.eval(&format!("(set-window-layout {bad})")).unwrap();
        assert_eq!(out.value.as_deref(), Some("#false"), "{bad}");
        assert!(out.commands.is_empty(), "{bad}");
    }
}

#[test]
fn restore_file_buffers_accepts_lists_and_dotted_pairs() {
    let (engine, _rx) = ScriptEngine::new();
    match commands(
        &engine,
        r#"(restore-file-buffers '(("/a" 3) ("/b" . 7) ("/c") (1 2)))"#,
    )
    .as_slice()
    {
        [EditorCommand::RestoreFileBuffers(files)] => assert_eq!(
            files,
            &[("/a".into(), 3), ("/b".into(), 7), ("/c".into(), 0)]
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(value(&engine, "(restore-file-buffers '())"), "");
}
