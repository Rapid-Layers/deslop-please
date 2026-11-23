//! End-to-end tests of `deslop-please` on small workspaces through `rust-analyzer scip`.
//! A fixture family is a base workspace in `tests/fixtures/<family>` and its
//! variants in `tests/variants/<family>/<variant>`, each holding only the files
//! it changes: restructurings that must report the same keys, and a fix.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TOOL: &str = "deslop-please";
const PROGRAM: &str = env!("CARGO_BIN_EXE_deslop-please");

/// A copy of `fixture`, with `variant`'s files over it, to run `test` in.
fn workspace(fixture: &str, variant: Option<&str>, test: &str) -> PathBuf {
    let rust_analyzer = Command::new("rust-analyzer").arg("--version").output();
    assert!(
        rust_analyzer.is_ok_and(|output| output.status.success()),
        "these tests run rust-analyzer, which is not on PATH (the dev shell has it)"
    );
    let copy = Path::new(env!("CARGO_TARGET_TMPDIR")).join(test);
    if copy.exists() {
        std::fs::remove_dir_all(&copy).unwrap();
    }
    let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
    copy_directory(&tests.join("fixtures").join(fixture), &copy);
    if let Some(variant) = variant {
        copy_directory(&tests.join("variants").join(fixture).join(variant), &copy);
    }
    copy
}

fn copy_directory(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let path = entry.unwrap().path();
        let target = to.join(path.file_name().unwrap());
        if path.is_dir() {
            copy_directory(&path, &target);
        } else {
            std::fs::copy(&path, &target).unwrap();
        }
    }
}

struct Run {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn run(workspace: &Path, arguments: &[&str]) -> Run {
    let Output {
        status,
        stdout,
        stderr,
    } = Command::new(PROGRAM)
        .args(arguments)
        .current_dir(workspace)
        // CI sets CARGO_TERM_COLOR=always; the tests read plain text.
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .unwrap();
    let run = Run {
        code: status.code(),
        stdout: String::from_utf8(stdout).unwrap(),
        stderr: String::from_utf8(stderr).unwrap(),
    };
    eprintln!(
        "$ {TOOL} {}\n{}{}",
        arguments.join(" "),
        run.stdout,
        run.stderr
    );
    run
}

/// `lint key` for every diagnostic in `workspace`, as the allow file lists them.
fn keys(workspace: &Path) -> Vec<String> {
    let allow = workspace.with_extension("keys");
    let update = run(workspace, &["--update-allow", allow.to_str().unwrap()]);
    assert_eq!(update.code, Some(0));
    let mut keys: Vec<String> = std::fs::read_to_string(&allow)
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_owned)
        .collect();
    keys.sort();
    keys
}

/// The keys of `family`'s base, or of one of its variants.
fn variant_keys(family: &str, variant: Option<&str>) -> Vec<String> {
    let test = format!("{family}-{}", variant.unwrap_or("base"));
    keys(&workspace(family, variant, &test))
}

#[test]
fn clean_workspace_exits_zero() {
    let workspace = workspace("clean", None, "clean");
    let result = run(&workspace, &[]);
    assert_eq!(result.code, Some(0));
    assert!(result.stdout.contains(&format!("{TOOL}: ok")));
}

#[test]
fn a_leaked_field_leaks_however_it_is_reached_and_not_once_the_type_does_the_work() {
    let run_queue = run(&workspace("field_leak", None, "field_leak"), &[]);
    assert_eq!(run_queue.code, Some(1));
    assert!(run_queue.stdout.contains(
        "error[field-leak]: `RunQueue::items` is known outside `RunQueue`: 4 places in 2 modules"
    ));
    // `backlog` and `drained` only read `head`.
    assert!(run_queue.stdout.contains(
        "    runqueue::queue::ring::RunQueue\n\
         \x20     items ◀━┳━ 2  runqueue::sched::policy\n\
         \x20             ┗━ 2  runqueue::sched::report\n\
         \x20     also known outside: head\n\
         \x20     kept: limit\n"
    ));
    let leaked = [
        "field-leak runqueue::queue::ring::RunQueue::head",
        "field-leak runqueue::queue::ring::RunQueue::items",
    ];
    for variant in [
        "getters",
        "let_getters",
        "all_pub",
        "pasted",
        "tuple_getter",
    ] {
        assert_eq!(
            variant_keys("field_leak", Some(variant)),
            leaked,
            "{variant}"
        );
    }
    // The work moved into the type, or into a type behind a field that it
    // passes each call on to; and reading a copy of a value, with or without
    // a getter, works on nothing.
    for variant in ["real", "passed_on", "read"] {
        assert_eq!(
            variant_keys("field_leak", Some(variant)),
            Vec::<String>::new(),
            "{variant}"
        );
    }
}

#[test]
fn a_module_cycle_stays_one_through_a_re_export_or_a_relay_until_code_moves_where_it_is_used() {
    let net = run(&workspace("module_cycle", None, "module_cycle"), &[]);
    assert_eq!(net.code, Some(1));
    assert!(
        net.stdout
            .contains("error[module-cycle]: 2 modules in `net` use each other in a circle")
    );
    assert!(!net.stdout.contains("error[tangle]"));
    // Things in a chain: moving `BACKLOG_MOST` to its one user breaks it.
    assert!(net.stdout.contains(
        "    ┌─▶ kinds\n\
         \x20   │     ┃ kinds.rs:10  Stream::new uses BACKLOG_MOST\n\
         \x20   │     ▼\n\
         \x20   │   socket\n\
         \x20   │     │ socket.rs:5  listen uses Stream::{self, new}\n\
         \x20   └─────┘\n"
    ));
    assert_eq!(
        variant_keys("module_cycle", Some("reexported")),
        ["module-cycle net::kinds,net::socket"]
    );
    assert_eq!(
        variant_keys("module_cycle", Some("relayed")),
        ["module-cycle net::kinds,net::limits,net::socket"]
    );
    assert_eq!(
        variant_keys("module_cycle", Some("moved")),
        Vec::<String>::new()
    );
}

#[test]
fn a_tangle_stays_one_however_it_is_laid_out_until_it_is_one_module_or_untied() {
    let tasks_and_timers = "tangle boot::kernel::tasks::Tasks,boot::kernel::timers::Timers";
    let cycle = "module-cycle boot::kernel::tasks,boot::kernel::timers";
    let boot = run(&workspace("tangle", None, "tangle"), &[]);
    assert!(
        boot.stdout
            .contains("error[tangle]: 2 things know each other in a circle, across 2 modules")
    );
    assert!(boot.stdout.contains(
        "    ┌─▶ boot::kernel::tasks::Tasks\n\
         \x20   │     ┃ tasks.rs:17   Tasks::spawn uses Timers::{self, arm}\n\
         \x20   │     ▼\n\
         \x20   │   boot::kernel::timers::Timers\n\
         \x20   │     │ timers.rs:16  Timers::fire uses Tasks::{self, wake}\n\
         \x20   └─────┘\n"
    ));
    assert!(
        boot.stdout.contains(
            "error[module-cycle]: 2 modules in `boot::kernel` use each other in a circle"
        )
    );
    // Moving an `impl` block or lifting a type into the parent's file puts the
    // modules in order, and leaves the tangle.
    assert_eq!(
        variant_keys("tangle", Some("impl_moved")),
        [tasks_and_timers]
    );
    assert_eq!(
        variant_keys("tangle", Some("lifted")),
        ["tangle boot::kernel::Tasks,boot::kernel::timers::Timers"]
    );
    // The figure's fix: `Tasks` stays lifted, and `tick` unties it from `Timers`.
    for variant in ["merged", "real", "lifted_untied"] {
        assert_eq!(
            variant_keys("tangle", Some(variant)),
            Vec::<String>::new(),
            "{variant}"
        );
    }

    let ring = workspace("tangle", Some("ring"), "tangle-ring");
    let result = run(&ring, &[]);
    assert!(
        result
            .stdout
            .contains("error[tangle]: 8 things know each other in a circle, all in `boot::ring`")
    );
    let ring_key = "tangle boot::ring::A,boot::ring::B,boot::ring::C,boot::ring::D,\
                    boot::ring::E,boot::ring::F,boot::ring::G,boot::ring::H";
    assert_eq!(keys(&ring), [cycle, tasks_and_timers, ring_key]);
}

#[test]
fn a_shallow_module_stays_shallow_however_it_is_dressed_until_it_does_more_with_less() {
    let store = run(&workspace("shallow", None, "shallow"), &[]);
    assert!(store.stdout.contains(
        "error[shallow-module]: `inventory::store` is shallow: it shows 9 of its 15 functions"
    ));
    assert!(store.stdout.contains(
        "= note: `Store` shows 9 functions: Store::contains, Store::get, Store::insert, \
         Store::is_full, Store::largest, Store::new, Store::remove, +2 more"
    ));
    assert!(store.stdout.contains(
        "                  7\n\
         \x20   shows  ███████│██ 9\n\
         \x20   hides  ██████ │ 6\n\
         \x20   Store  ███████│██ 9\n"
    ));
    let shallow = ["shallow-module inventory::store"];
    // Some or all of its methods made free functions that use the store:
    // they are still the store's functions.
    for variant in [
        "padded",
        "impls_moved",
        "pub_field",
        "private_modules",
        "some_free_functions",
        "free_functions",
    ] {
        assert_eq!(variant_keys("shallow", Some(variant)), shallow, "{variant}");
    }
    // `is_full` names the store only in its signature.
    let free = run(
        &workspace("shallow", Some("free_functions"), "shallow-free"),
        &[],
    );
    assert!(free.stdout.contains(
        "= note: `Store` shows 9 functions: Store::new, contains, get, insert, is_full, \
         largest, remove, +2 more"
    ));
    assert_eq!(variant_keys("shallow", Some("deep")), Vec::<String>::new());
}

#[test]
fn small_types_that_each_show_a_few_functions_are_not_shallow_however_many_they_show_in_all() {
    let stats = run(&workspace("small_types", None, "small_types"), &[]);
    assert!(stats.stdout.contains(&format!("{TOOL}: ok")));
    assert_eq!(stats.code, Some(0));
}

/// The `clean` fixture's `src/socket/` in `workspace`, its `mod.rs` declaring
/// `count` modules, each in a file of its own, before `mod_rs`.
fn socket_parts(workspace: &Path, mod_rs: &str, count: usize) {
    let socket = workspace.join("src/socket");
    let declared: String = (1..=count).map(|n| format!("mod part{n};\n")).collect();
    std::fs::write(socket.join("mod.rs"), declared + mod_rs).unwrap();
    for n in 1..=count {
        let part = format!("pub struct Part{n};\n");
        std::fs::write(socket.join(format!("part{n}.rs")), part).unwrap();
    }
}

#[test]
fn a_long_file_and_a_wide_directory_are_reported_until_they_are_split() {
    let workspace = workspace("clean", None, "layout");
    let kinds = workspace.join("src/kinds.rs");
    let original = std::fs::read_to_string(&kinds).unwrap();
    let padded = original.clone() + &"\n".repeat(2_000 - original.lines().count());
    std::fs::write(&kinds, &padded).unwrap();
    let socket_mod = std::fs::read_to_string(workspace.join("src/socket/mod.rs")).unwrap();
    // 2,000 lines, and 10 files: at the most, not past it.
    socket_parts(&workspace, &socket_mod, 8);
    assert_eq!(keys(&workspace), Vec::<String>::new());

    socket_parts(&workspace, &socket_mod, 9);
    std::fs::write(&kinds, padded + "pub struct Late;\n").unwrap();
    let result = run(&workspace, &[]);
    assert_eq!(result.code, Some(1));
    assert!(result.stdout.contains(
        "error[wide-directory]: `src/socket/` is wide: it holds 11 files and directories, \
         more than 10\n\
         \x20 --> src/socket/mod.rs:1:1\n\
         \x20 = note: 11 files: datagram.rs, mod.rs, part1.rs, part2.rs, part3.rs, part4.rs, \
         part5.rs, +4 more\n\
         \x20                   10\n\
         \x20   holds  ██████████│█ 11\n"
    ));
    assert!(result.stdout.contains(
        "error[long-file]: `src/kinds.rs` is long: it has 2001 lines, more than 2000\n\
         \x20 --> src/kinds.rs:1:1\n"
    ));
    assert_eq!(
        keys(&workspace),
        ["long-file src/kinds.rs", "wide-directory src/socket/"]
    );
}

#[test]
fn a_module_behind_a_cfg_that_rust_analyzer_leaves_off_counts_in_the_layout() {
    let workspace = workspace("clean", None, "layout_behind_a_cfg");
    let socket = workspace.join("src/socket");
    let socket_mod = std::fs::read_to_string(socket.join("mod.rs")).unwrap();
    let checks = "#[cfg(all(test, my_checks))]\nmod checks;\n".to_owned() + &socket_mod;
    socket_parts(&workspace, &checks, 8);
    std::fs::create_dir(socket.join("checks")).unwrap();
    std::fs::write(socket.join("checks/mod.rs"), "mod long;\n").unwrap();
    std::fs::write(socket.join("checks/long.rs"), "\n".repeat(2_001)).unwrap();
    assert_eq!(
        keys(&workspace),
        [
            "long-file src/socket/checks/long.rs",
            "wide-directory src/socket/"
        ]
    );
}

#[test]
fn the_allow_file_suppresses_only_existing_entries_and_notes_what_is_fixed() {
    let workspace = workspace("field_leak", None, "allow");
    let allow = workspace.with_extension("allow");
    let allow = allow.to_str().unwrap();
    assert_eq!(run(&workspace, &["--update-allow", allow]).code, Some(0));
    let allowed = run(&workspace, &["--allow", allow]);
    assert_eq!(allowed.code, Some(0));
    assert!(allowed.stdout.contains(&format!("{TOOL}: ok")));

    let real = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/variants/field_leak/real");
    copy_directory(&real, &workspace);
    let fixed = run(&workspace, &["--allow", allow]);
    assert_eq!(fixed.code, Some(0));
    assert!(fixed.stdout.contains("2 allowed entries"));
    assert!(fixed.stdout.contains("now fixed"));

    let lib = workspace.join("src/lib.rs");
    let ring = "\npub mod ring {\n    pub struct A(pub Option<Box<B>>);\n    pub struct B(pub Option<Box<C>>);\n\
                pub struct C(pub Option<Box<D>>);\n    pub struct D(pub Option<Box<E>>);\n\
                pub struct E(pub Option<Box<F>>);\n    pub struct F(pub Option<Box<G>>);\n\
                pub struct G(pub Option<Box<H>>);\n    pub struct H(pub Option<Box<A>>);\n}\n";
    let with_ring = std::fs::read_to_string(&lib).unwrap() + ring;
    std::fs::write(&lib, with_ring).unwrap();
    let new = run(&workspace, &["--allow", allow]);
    assert_eq!(new.code, Some(1));
    assert!(new.stdout.contains("error[tangle]: 8 things"));
    assert!(!new.stdout.contains("field-leak"));
}

#[test]
fn across_crates_a_field_leaks_and_test_code_is_left_out() {
    let workspace = workspace("two_crates", None, "two_crates");
    assert_eq!(run(&workspace, &[]).code, Some(0));

    let lib = workspace.join("crates/lib/src/lib.rs");
    let vault = "\npub struct Plain { pub x: u32, pub y: u32 }\n\
                 pub struct Vault { secret: u32, pub leaked: u32 }\n\
                 impl Vault {\n    pub fn new() -> Self { Self { secret: 1, leaked: 2 } }\n    \
                 pub fn next(&self) -> u32 { self.secret + 1 }\n}\n\
                 #[cfg(test)]\nmod tests {\n    #[test]\n    fn peeks() { let _ = super::Vault::new().leaked; }\n}\n";
    std::fs::write(&lib, std::fs::read_to_string(&lib).unwrap() + vault).unwrap();
    let it = workspace.join("app/tests/it.rs");
    let peeks = "\n#[test]\nfn peeks() {\n    let _ = lib::Vault::new().leaked;\n}\n";
    std::fs::write(&it, std::fs::read_to_string(&it).unwrap() + peeks).unwrap();

    let main = workspace.join("app/src/main.rs");
    let original = std::fs::read_to_string(&main).unwrap();
    let plain = format!("{original}\nfn bump_plain(p: &mut lib::Plain) {{ p.x += 1; }}\n");
    std::fs::write(&main, plain).unwrap();
    assert_eq!(keys(&workspace), Vec::<String>::new());
    let read = format!("{original}\nfn read_vault(v: &lib::Vault) -> u32 {{ v.leaked }}\n");
    std::fs::write(&main, read).unwrap();
    assert_eq!(keys(&workspace), Vec::<String>::new());

    let leak = format!("{original}\nfn bump_vault(v: &mut lib::Vault) {{ v.leaked += 1; }}\n");
    std::fs::write(&main, leak).unwrap();
    let result = run(&workspace, &[]);
    assert_eq!(result.code, Some(1));
    assert!(result.stdout.contains(
        "error[field-leak]: `Vault::leaked` is known outside `Vault`: 1 place in 1 module"
    ));
    assert_eq!(keys(&workspace), ["field-leak lib::Vault::leaked"]);
}

#[test]
fn outside_a_cargo_workspace_reports_error() {
    let empty = std::env::temp_dir().join(format!("{TOOL}-not-a-workspace-{}", std::process::id()));
    std::fs::create_dir_all(&empty).unwrap();
    let run = run(&empty, &[]);
    std::fs::remove_dir_all(&empty).unwrap();
    assert_eq!(run.code, Some(2));
    assert!(run.stderr.contains("not inside a Cargo workspace"));
}
