//! Running one program, separated from printing its result.
//!
//! [`execute_to`] takes source text and gives back everything the two output
//! modes of `run` need: whether the run succeeded, the lines the program
//! printed, the check diagnostics if it never ran, and the fault if it
//! started and could not finish. Keeping this apart from `main` is what lets
//! a test assert the exact text a program prints, which is the only way to
//! catch an instruction that runs without faulting but produces the wrong
//! value.

use crate::default_grants::{Output, bind_capability, capabilities_for};
use crate::granted::Granted;
use hollowscript::{Diagnostic, Fault};

/// What one `run` produced.
pub(crate) struct RunResult {
    /// True when the program ran to completion without a fault.
    pub ok: bool,
    /// The lines the program passed to `print`, in order, when they were
    /// collected (`PrintSink::collect`); empty when they were streamed.
    pub output: Vec<String>,
    /// The check diagnostics, when the program never ran because it did not
    /// check. Empty otherwise.
    pub diagnostics: Vec<Diagnostic>,
    /// The fault, when the program started and could not finish.
    pub fault: Option<Fault>,
}

/// Checks, compiles, verifies and runs `source` with `granted` in force and
/// section 11's default limits, keeping what it prints in
/// [`RunResult::output`].
/// `file` is the path the entry module was given as; its directory is the
/// program's root (section 10.3), so an import resolves against that
/// directory and not against the working directory.
#[cfg(test)]
pub(crate) fn execute(source: &str, file: &str, granted: Granted) -> RunResult {
    execute_with_limits(source, file, granted, hollowscript::Limits::default())
}

/// As [`execute`], with `limits` in place of section 11's defaults: for a
/// test that needs a small `maxMemoryBytes` or similar to reach a limit
/// without an equally large fixture.
#[cfg(test)]
pub(crate) fn execute_with_limits(
    source: &str,
    file: &str,
    granted: Granted,
    limits: hollowscript::Limits,
) -> RunResult {
    execute_to(
        source,
        file,
        granted,
        limits,
        &crate::default_grants::PrintSink::collect(),
    )
}

/// Checks, compiles, verifies and runs `source`, sending what it prints to
/// `output`: [`crate::default_grants::PrintSink::collect`] keeps it in [`RunResult::output`],
/// [`crate::default_grants::PrintSink::stream`] writes each line to standard output as it happens
/// and leaves [`RunResult::output`] empty.
pub(crate) fn execute_to(
    source: &str,
    file: &str,
    granted: Granted,
    limits: hollowscript::Limits,
    output: &Output,
) -> RunResult {
    let program = crate::loader::Program::at(file);
    let options = options(limits, &granted);
    let granted = std::rc::Rc::new(granted);
    let outcome = hollowscript::load(
        source,
        program.entry(),
        &options,
        &mut program.loader(),
        &mut |name, function| bind_capability(name, function, output, &granted),
        &mut hollowscript::Control::none(),
    );
    finish(outcome, output)
}

/// As [`execute_to`], for a bytecode container `build` produced rather than
/// source text: verified exactly as a VM with no compiler verifies one, and
/// refused (HS0404, as a check diagnostic) if it uses a capability this run
/// does not grant with the same signature.
pub(crate) fn execute_bytecode(
    bytes: &[u8],
    granted: Granted,
    limits: hollowscript::Limits,
    output: &Output,
) -> Result<RunResult, hollowscript::BytecodeError> {
    let options = options(limits, &granted);
    let granted = std::rc::Rc::new(granted);
    let outcome = hollowscript::load_bytecode(
        bytes,
        &options,
        &mut |name, function| bind_capability(name, function, output, &granted),
        &mut hollowscript::Control::none(),
    )?;
    Ok(finish(outcome, output))
}

fn options(limits: hollowscript::Limits, granted: &Granted) -> hollowscript::Options {
    hollowscript::Options {
        limits,
        capabilities: capabilities_for(granted),
        filter_schema: None,
        // The command line runs capabilities synchronously: it owns the
        // thread for the whole run and has nothing to do between frames.
        batch_capability_calls: false,
        gc_stress_mode: false,
    }
}

fn finish(outcome: hollowscript::LoadOutcome, output: &Output) -> RunResult {
    match outcome {
        hollowscript::LoadOutcome::CheckFailed(diagnostics) => RunResult {
            ok: false,
            output: Vec::new(),
            diagnostics,
            fault: None,
        },
        hollowscript::LoadOutcome::Instance(mut instance) => {
            let fault = instance.fault().cloned();
            let _ = instance.dispose();
            RunResult {
                ok: fault.is_none(),
                output: output.lines(),
                diagnostics: Vec::new(),
                fault,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{execute, execute_with_limits};
    use crate::granted::Granted;
    use hollowscript::Diagnostic;

    /// Runs `source` and returns the lines it printed, failing the test with
    /// the diagnostics or the fault if it did not run to completion.
    fn printed(source: &str) -> Vec<String> {
        let result = execute(source, "test.hws", Granted::none());
        assert!(
            result.ok,
            "the program did not run: diagnostics {:?}, fault {:?}",
            result
                .diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.code, d.message))
                .collect::<Vec<_>>(),
            result.fault.as_ref().map(|f| f.to_diagnostic().message)
        );
        result.output
    }

    #[test]
    fn a_program_prints_what_it_computes() {
        assert_eq!(
            printed("uses print\neffect print(\"hello\")\nreturn 0\n"),
            ["hello"]
        );
    }

    #[test]
    fn interpolation_renders_every_type_section_3_9_allows() {
        // The Float column is the worked example of section 3.9 itself.
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "effect print(\"${1.0} ${100.0} ${1.5} ${2 / 3} ${1.0e20} ${-0.000001}\")\n",
                "effect print(\"${true} ${false} ${-7} ${0}\")\n",
                "effect print(\"a${\"b\"}c\")\n",
                "return 0\n",
            )),
            [
                "1.0 100.0 1.5 0.6666666666666666 1e20 -1e-6",
                "true false -7 0",
                "abc",
            ]
        );
    }

    #[test]
    fn interpolating_a_variable_that_a_loop_changed_shows_its_current_value() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "variable total: Int = 0\n",
                "for n in [1, 2, 3, 4] {\n",
                "    total = total + n\n",
                "}\n",
                "effect print(\"total ${total}\")\n",
                "return total\n",
            )),
            ["total 10"]
        );
    }

    #[test]
    fn a_program_runs_its_imports() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-execute-import-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("util.hws"), "export constant two = 2\n").unwrap();
        std::fs::write(dir.join("sub/three.hws"), "export constant three = 3\n").unwrap();
        let entry = dir.join("main.hws");
        let source = concat!(
            "uses print\n",
            "import two from \"./util\"\n",
            "import three from \"./sub/three\"\n",
            "effect print(\"${two} ${three}\")\n",
            "return two + three\n",
        );
        std::fs::write(&entry, source).unwrap();
        let result = execute(source, &entry.to_string_lossy(), Granted::none());
        assert!(
            result.ok,
            "diagnostics {:?}",
            result
                .diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.code, d.message))
                .collect::<Vec<_>>()
        );
        assert_eq!(result.output, ["2 3"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Section 10.3: the root is the entry module's own directory, so an
    /// import that climbs above it is HS0605 and the file is never read.
    #[test]
    fn an_import_above_the_root_is_refused_even_when_the_file_exists() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-execute-escape-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(dir.join("program")).unwrap();
        std::fs::write(dir.join("secret.hws"), "export constant s = 1\n").unwrap();
        let entry = dir.join("program/main.hws");
        let source = "import s from \"../secret\"\nreturn s\n";
        std::fs::write(&entry, source).unwrap();
        let result = execute(source, &entry.to_string_lossy(), Granted::none());
        assert!(!result.ok);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == hollowscript::Code::HS0605),
            "{:?}",
            result.diagnostics
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Section 11.5 requires runaway recursion to reach the language's own
    /// counter before it can exhaust the implementation's stack. A real
    /// program, through the whole pipeline, in whatever build this test runs
    /// in.
    #[test]
    fn runaway_recursion_faults_rather_than_aborting_the_process() {
        let result = execute(
            "fn down(n: Int): Int {\n    return down(n + 1)\n}\nreturn down(0)\n",
            "test.hws",
            Granted::none(),
        );
        assert!(!result.ok);
        let fault = result.fault.expect("a runaway recursion faults");
        assert_eq!(fault.to_diagnostic().code, hollowscript::Code::HS1005);
    }

    /// And a deep but terminating recursion runs, rather than faulting or
    /// aborting, right up to the limit.
    #[test]
    fn a_deep_but_terminating_recursion_runs_to_the_end() {
        let result = execute(
            concat!(
                "uses print\n",
                "fn down(n: Int): Int {\n",
                "    if n <= 0 { return 0 }\n",
                "    return down(n - 1) + 1\n",
                "}\n",
                "effect print(\"${down(900)}\")\n",
                "return 0\n",
            ),
            "test.hws",
            Granted::none(),
        );
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["900"]);
    }

    /// The standard library, through the whole pipeline, against the worked
    /// examples of sections 13.2, 13.3, 13.4, 14.1, 14.2, 14.3 and 13.5.
    /// Every expected line below is copied from the specification's own
    /// output block for that example.
    #[test]
    fn the_standard_library_produces_the_specifications_own_output() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "constant xs = [3, 1, 2]\n",
                "effect print(\"${xs.length()} ${xs.isEmpty()} ${xs.contains(2)} ${xs.contains(7)}\")\n",
                "constant t = \"  Hello, World  \".trim()\n",
                "effect print(\"${t.length()} ${t.byteLength()} ${t.toUpper()} ${t.substring(7, 100)}\")\n",
                "effect print(\"${\"a😀b\".length()} ${\"a😀b\".byteLength()} ${\"a😀b\".split(\"\").length()}\")\n",
                "effect print(\"${\"a,b,,c\".split(\",\").length()} ${\"x\".repeat(3)} ${\"7\".padStart(3, \"0\")}\")\n",
                "effect print(\"${round(-2.5)} ${round(2.5)} ${floor(-0.5)} ${ceil(0.2)} ${trunc(-3.9)}\")\n",
                "effect print(\"${abs(-3)} ${abs(-2.5)} ${min(1, 2.5)} ${max(3, 9)} ${clamp(15, 0, 10)}\")\n",
                "effect print(\"${div(7, 2)} ${mod(-7, 3)} ${remainder(-7, 3)} ${ipow(2, 62)}\")\n",
                "effect print(\"${sqrt(16)} ${sqrt(2)} ${pow(2, 10)} ${toFloat(3)}\")\n",
                "constant m = [\"b\": 2, \"a\": 1]\n",
                "constant merged = m.merge([\"a\": 10, \"c\": 3])\n",
                "effect print(\"${merged.keys().join(\",\")} ${merged.values().length()} ${merged.get(\"a\") == 10} ${merged.has(\"z\")}\")\n",
                "constant built = mapFrom([{ key: 1, value: \"one\" }, { key: 1, value: \"uno\" }, { key: 2, value: \"two\" }])\n",
                "effect print(\"${built.length()} ${built.get(1) == \"uno\"}\")\n",
                "constant a = set([1, 2, 3])\n",
                "constant b = set([3, 4])\n",
                "effect print(\"${a.union(b).length()} ${a.intersect(b).toList().first() == 3} ${a.difference(b).length()} ${a.has(2)}\")\n",
                "record Task = { title: String, priority: Int }\n",
                "constant task: Task = { title: \"Ship\", priority: 2 }\n",
                "effect print(\"${keys(task).join(\",\")} ${has(task, \"title\")} ${has(task, \"owner\")}\")\n",
                "return 0\n",
            )),
            [
                "3 false true false",
                "12 12 HELLO, WORLD World",
                "3 6 3",
                "4 xxx 007",
                "-3 3 -1 1 -3",
                "3 2.5 1.0 9 10",
                "3 2 -1 4611686018427387904",
                "4.0 1.4142135623730951 1024.0 3.0",
                "a,b,c 3 true false",
                "2 true",
                "4 true 2 true",
                "priority,title true false",
            ]
        );
    }

    /// The library functions that call a closure, and the worked output of
    /// `corpus/examples/digest.hws`, which uses `map`, `join`, `isEmpty`,
    /// `length` and a `match` on an optional field inside a `for`.
    #[test]
    fn the_library_functions_that_call_a_closure_run() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "record Task = { id: String, title: String, priority: String, owner: String? }\n",
                "fn titlesOf(items: [Task]): [String] {\n",
                "    return items.map(fn(t: Task): String { return t.title })\n",
                "}\n",
                "fn ownerOf(task: Task): String {\n",
                "    match task.owner {\n",
                "        none { return \"unassigned\" }\n",
                "        else { return task.owner }\n",
                "    }\n",
                "}\n",
                "constant urgent: [Task] = [\n",
                "    { id: \"1\", title: \"Fix build\", priority: \"high\", owner: \"sam\" },\n",
                "    { id: \"2\", title: \"Write docs\", priority: \"high\", owner: none }\n",
                "]\n",
                "if urgent.isEmpty() {\n",
                "    effect print(\"nothing urgent\")\n",
                "    return 0\n",
                "}\n",
                "for task in urgent {\n",
                "    effect print(\"notify ${ownerOf(task)}: ${task.title}\")\n",
                "}\n",
                "effect print(titlesOf(urgent).join(\", \"))\n",
                "effect print(\"${[1, 2, 3].reduce(0, fn(a: Int, x: Int): Int { return a + x })}\")\n",
                "effect print(\"${[1, 2, 3, 4].filter(fn(n: Int): Bool { return n > 2 }).length()}\")\n",
                "effect print(\"${[1, 2].any(fn(n: Int): Bool { return n > 1 })} ${[1, 2].all(fn(n: Int): Bool { return n > 0 })}\")\n",
                "constant m = [\"a\": 1, \"b\": 2]\n",
                "effect print(\"${m.filter(fn(k: String, v: Int): Bool { return v > 1 }).length()} ${m.mapValues(fn(v: Int): String { return \"${v}\" }).get(\"a\") == \"1\"}\")\n",
                "return urgent.length()\n",
            )),
            [
                "notify sam: Fix build",
                "notify unassigned: Write docs",
                "Fix build, Write docs",
                "6",
                "2",
                "true true",
                "1 true",
            ]
        );
    }

    /// A callback that itself calls a library function that takes a callback
    /// nests on the interpreter's own frames, not the host's stack, so it is
    /// bounded by `maxCallDepth` like any other call.
    #[test]
    fn a_callback_may_call_another_library_function_with_a_callback() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "fn total(rows: [[Int]]): Int {\n",
                "    return rows.map(fn(row: [Int]): Int {\n",
                "        return row.reduce(0, fn(a: Int, x: Int): Int { return a + x })\n",
                "    }).reduce(0, fn(a: Int, x: Int): Int { return a + x })\n",
                "}\n",
                "effect print(\"${total([[1, 2], [3, 4], [5]])}\")\n",
                "return 0\n",
            )),
            ["15"]
        );
    }

    /// A `for` loop keeps its iterator's state in registers of the
    /// enclosing function, so a call in the body must not be given those
    /// registers as temporaries. Before the compiler reserved them, the
    /// first iteration ran and the second read whatever the body had left
    /// behind.
    #[test]
    fn a_call_in_a_loop_body_does_not_disturb_the_loop() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "fn twice(n: Int): Int { return n * 2 }\n",
                "for x in [1, 2, 3] {\n",
                "    effect print(\"${twice(x)}\")\n",
                "}\n",
                "constant m = [\"a\": 1, \"b\": 2]\n",
                "for entry in m {\n",
                "    effect print(\"${twice(entry.value)}\")\n",
                "}\n",
                "for x in [1, 2] {\n",
                "    for y in [1, 2] {\n",
                "        effect print(\"${twice(x) + y}\")\n",
                "    }\n",
                "}\n",
                "return 0\n",
            )),
            ["2", "4", "6", "2", "4", "3", "4", "5", "6"]
        );
    }

    /// `--allow env` and `--allow args` grant real capabilities, and a name
    /// outside an `env` grant reads as `none`, which section 9.5 rule 3
    /// makes indistinguishable from an unset variable. `PATH` is used
    /// because it is set for every process this can run in, and the test
    /// never changes the environment it runs in.
    #[test]
    fn env_and_args_are_granted_and_scoped() {
        let source = concat!(
            "uses print, env, args\n",
            "fn seen(v: String?): String {\n",
            "    if v == none { return \"unset\" }\n",
            "    return \"set\"\n",
            "}\n",
            "effect print(seen(effect env(\"PATH\")))\n",
            "effect print((effect args()).join(\",\"))\n",
            "return 0\n",
        );

        // Not granted at all: the program does not even check.
        let refused = execute(source, "test.hws", Granted::none());
        assert!(!refused.ok);
        assert!(
            refused
                .diagnostics
                .iter()
                .any(|d| d.code == hollowscript::Code::HS0404),
            "{:?}",
            refused.diagnostics
        );

        let granted = |scope: &str| {
            Granted::from_flags(
                &[
                    crate::grants::parse_grant(scope).unwrap(),
                    crate::grants::parse_grant("args").unwrap(),
                ],
                &["one".to_owned(), "two".to_owned()],
            )
            .expect("both flags are valid")
        };

        let inside = execute(source, "test.hws", granted("env=PATH"));
        assert!(inside.ok, "fault {:?}", inside.fault);
        assert_eq!(inside.output, ["set", "one,two"]);

        // The same variable, the same process, a grant that does not name
        // it: the program cannot tell it from an unset one.
        let outside = execute(source, "test.hws", granted("env=SOMETHING_ELSE"));
        assert!(outside.ok, "fault {:?}", outside.fault);
        assert_eq!(outside.output, ["unset", "one,two"]);

        // No scope at all reads every variable.
        let unscoped = execute(source, "test.hws", granted("env"));
        assert!(unscoped.ok, "fault {:?}", unscoped.fault);
        assert_eq!(unscoped.output, ["set", "one,two"]);
    }

    /// A namespace capability call (`clock.now`, `random.int`) is a
    /// capability call, not a field read followed by a generic call. Before
    /// the compiler emitted one, every such call built a function value from
    /// an instruction the interpreter cannot run, which failed verification.
    #[test]
    fn a_namespace_capability_is_called_not_read_as_a_field() {
        assert_eq!(
            printed(concat!(
                "uses print, clock, random\n",
                "effect print(effect clock.now())\n",
                "effect print(effect clock.today())\n",
                "effect print(\"${effect clock.unixMillis() > 0} ${effect random.int(1, 7) >= 1}\")\n",
                "return 0\n",
            ))
            .len(),
            3
        );
    }

    /// `fs` end to end: a granted read, a read of a path outside every
    /// grant, which section 9.5 rule 3 makes `Err(Denied)`, a write into a
    /// separately granted directory, and reading it back. The error variants
    /// are matched by name, which is only possible because the host's own
    /// `IoError` is a type the program can write.
    #[test]
    fn fs_reads_and_writes_within_its_grants_and_refuses_outside_them() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-fs-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(dir.join("in")).unwrap();
        std::fs::create_dir_all(dir.join("out")).unwrap();
        std::fs::create_dir_all(dir.join("secret")).unwrap();
        std::fs::write(dir.join("in/readable.txt"), "contents").unwrap();
        std::fs::write(dir.join("secret/private.txt"), "not for you").unwrap();

        let source = concat!(
            "uses print, fs\n",
            "fn show(r: Result<String, IoError>): String {\n",
            "    match r {\n",
            "        Ok(text) { return \"ok:${text}\" }\n",
            "        Err(NotFound(p)) { return \"notfound\" }\n",
            "        Err(Denied(p)) { return \"denied\" }\n",
            "        Err(e) { return \"other\" }\n",
            "    }\n",
            "}\n",
            "effect print(show(effect fs.read(\"IN/readable.txt\")))\n",
            "effect print(show(effect fs.read(\"SECRET/private.txt\")))\n",
            "effect print(show(effect fs.read(\"IN/missing.txt\")))\n",
            "match effect fs.write(\"OUT/made.txt\", \"written\") {\n",
            "    Ok(n) { effect print(\"wrote ${n}\") }\n",
            "    Err(e) { effect print(\"write failed\") }\n",
            "}\n",
            "effect print(show(effect fs.read(\"OUT/made.txt\")))\n",
            "return 0\n",
        )
        .replace("IN/", &format!("{}/", dir.join("in").display()))
        .replace("OUT/", &format!("{}/", dir.join("out").display()))
        .replace("SECRET/", &format!("{}/", dir.join("secret").display()));

        let mut granted = Granted::none();
        granted.fs_read = vec![
            crate::capabilities::ScopeRoot::new(dir.join("in")).unwrap(),
            crate::capabilities::ScopeRoot::new(dir.join("out")).unwrap(),
        ];
        granted.fs_write = vec![crate::capabilities::ScopeRoot::new(dir.join("out")).unwrap()];

        let result = execute(&source, "test.hws", granted);
        assert!(
            result.ok,
            "diagnostics {:?}, fault {:?}",
            result
                .diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.code, d.message))
                .collect::<Vec<_>>(),
            result.fault.as_ref().map(|f| f.to_diagnostic().message)
        );
        assert_eq!(
            result.output,
            ["ok:contents", "denied", "notfound", "wrote 7", "ok:written",]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A write into a directory granted only for reading is refused, which
    /// is section 9.5 rule 3's "one grant does not imply another".
    #[test]
    fn a_write_needs_a_write_grant() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-fs-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let source = format!(
            concat!(
                "uses print, fs\n",
                "match effect fs.write(\"{}/out.txt\", \"x\") {{\n",
                "    Ok(n) {{ effect print(\"wrote\") }}\n",
                "    Err(Denied(p)) {{ effect print(\"denied\") }}\n",
                "    Err(e) {{ effect print(\"other\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            dir.display()
        );
        let mut granted = Granted::none();
        granted.fs_read = vec![crate::capabilities::ScopeRoot::new(&dir).unwrap()];
        let result = execute(&source, "test.hws", granted);
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["denied"]);
        assert!(!dir.join("out.txt").exists(), "nothing was written");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Section 16.8: "a file larger than the remaining memory allowance
    /// faults with HS1002 before its contents are read", end to
    /// end: a sparse file reporting a size in the gigabytes against a tiny
    /// `maxMemoryBytes` faults almost instantly rather than the run trying
    /// (and, before the fix, succeeding for a while) to buffer it all.
    #[test]
    fn fs_read_of_a_file_larger_than_max_memory_bytes_faults_hs1002() {
        let dir =
            std::env::temp_dir().join(format!("hollowscript-cli-fs-hs1002-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("huge.txt");
        {
            let file = std::fs::File::create(&path).unwrap();
            file.set_len(4 * 1024 * 1024 * 1024).unwrap(); // 4 GiB, sparse
        }
        let source = format!(
            concat!(
                "uses print, fs\n",
                "match effect fs.read(\"{}\") {{\n",
                "    Ok(t) {{ effect print(\"ok\") }}\n",
                "    Err(e) {{ effect print(\"err\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            path.display()
        );
        let mut granted = Granted::none();
        granted.fs_read = vec![crate::capabilities::ScopeRoot::new(&dir).unwrap()];
        let limits = hollowscript::Limits {
            max_memory_bytes: 1_000_000,
            ..hollowscript::Limits::default()
        };
        let result = execute_with_limits(&source, "test.hws", granted, limits);
        assert!(
            !result.ok,
            "expected a fault, got output {:?}",
            result.output
        );
        let fault = result.fault.expect("a fault, not a check failure");
        assert_eq!(fault.to_diagnostic().code.to_string(), "HS1002");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// `list.append` used to clone its receiver twice: once building an
    /// exact-capacity copy (`to_vec`), then again the moment `push` found no
    /// spare capacity and grew it (see `natives.rs`'s own comment on
    /// `list.append`). A loop of sequential appends is still O(n) per call
    /// (each still copies the whole list; only a persistent, structurally
    /// shared representation, or proving the receiver unique so it can be
    /// grown in place, would change that, and neither is done here: see
    /// `docs/design/performance.md`'s "Measured: a loop that keeps what it
    /// builds"), so this cannot assert the loop stays sub-quadratic. What it
    /// guards is the constant factor: 40,000 appends must not take anywhere
    /// near what a regression back to the double copy would cost. Timed
    /// rather than counted (there is nothing else here to count against),
    /// with a bound wide enough not to flake under CI load: the fixed
    /// version measures well under a second on ordinary developer hardware.
    #[test]
    fn list_append_does_not_copy_its_receiver_twice() {
        let start = std::time::Instant::now();
        let output = printed(concat!(
            "uses print\n",
            "variable xs: [Int] = []\n",
            "variable i = 0\n",
            "while i < 40000 {\n",
            "    xs = xs.append(i)\n",
            "    i = i + 1\n",
            "}\n",
            "effect print(\"${xs.length()}\")\n",
            "return 0\n",
        ));
        let elapsed = start.elapsed();
        assert_eq!(output, ["40000"]);
        assert!(
            elapsed < std::time::Duration::from_secs(10),
            "40,000 appends took {elapsed:?}, far more than the single-copy-per-call cost should"
        );
    }

    /// Value semantics (section 6): a list assigned to a second binding
    /// before `append` is called must not be affected by it, even though
    /// `append` now sizes its own copy up front instead of relying on
    /// `to_vec` to do it (the same guarantee `map.with` and `set.with`
    /// below must keep after the equivalent change to them).
    #[test]
    fn append_leaves_a_shared_list_untouched() {
        let output = printed(concat!(
            "uses print\n",
            "variable xs: [Int] = [1, 2, 3]\n",
            "constant shared = xs\n",
            "xs = xs.append(4)\n",
            "effect print(\"${shared.length()}\")\n",
            "effect print(\"${xs.length()}\")\n",
            "return 0\n",
        ));
        assert_eq!(output, ["3", "4"]);
    }

    /// The same guarantee as `append_leaves_a_shared_list_untouched`, for
    /// `Map.with` and `Set.with` (section 14.2 and 14.3), whose own copies
    /// are now also sized up front for the entry or element being added.
    #[test]
    fn map_and_set_with_leave_a_shared_collection_untouched() {
        let output = printed(concat!(
            "uses print\n",
            "variable m: Map<String, Int> = [\"a\": 1]\n",
            "constant sharedMap = m\n",
            "m = m.with(\"b\", 2)\n",
            "effect print(\"${sharedMap.length()}\")\n",
            "effect print(\"${m.length()}\")\n",
            "variable s: Set<Int> = set([1, 2])\n",
            "constant sharedSet = s\n",
            "s = s.with(3)\n",
            "effect print(\"${sharedSet.length()}\")\n",
            "effect print(\"${s.length()}\")\n",
            "return 0\n",
        ));
        assert_eq!(output, ["1", "2", "2", "3"]);
    }

    /// Reading a FIFO does not block the run indefinitely waiting
    /// for a writer that never comes; it is refused outright.
    #[cfg(unix)]
    #[test]
    fn fs_read_of_a_fifo_is_refused_not_a_hang() {
        let dir = std::env::temp_dir().join(format!(
            "hollowscript-cli-fs-fifo-e2e-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("pipe");
        let Ok(status) = std::process::Command::new("mkfifo").arg(&fifo).status() else {
            eprintln!("'mkfifo' is not available; skipping");
            std::fs::remove_dir_all(&dir).ok();
            return;
        };
        assert!(status.success(), "mkfifo failed");
        let source = format!(
            concat!(
                "uses print, fs\n",
                "match effect fs.read(\"{}\") {{\n",
                "    Ok(t) {{ effect print(\"ok\") }}\n",
                "    Err(Invalid(m)) {{ effect print(\"invalid\") }}\n",
                "    Err(e) {{ effect print(\"other\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            fifo.display()
        );
        let mut granted = Granted::none();
        granted.fs_read = vec![crate::capabilities::ScopeRoot::new(&dir).unwrap()];
        // No writer ever opens the other end of the FIFO: were `fs.read` to
        // block waiting for one, this test would hang rather than finish.
        let result = execute(&source, "test.hws", granted);
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["invalid"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Section 19.3 locates a run-stage diagnostic at the line where the
    /// failing operation starts. A fault carried no location at all until
    /// the compiler wrote a span table and the interpreter read it.
    #[test]
    fn a_fault_is_located_at_the_operation_that_raised_it() {
        let cases = [
            (
                concat!(
                    "uses print\n",
                    "effect print(\"a\")\n",
                    "constant x = div(1, 0)\n",
                    "return 0\n",
                ),
                3,
            ),
            (
                concat!(
                    "constant a = 1\n",
                    "constant b = 2\n",
                    "return 9223372036854775807 + 1\n",
                ),
                3,
            ),
            (
                concat!(
                    "fn down(n: Int): Int {\n",
                    "    return down(n + 1)\n",
                    "}\n",
                    "return down(0)\n",
                ),
                2,
            ),
            (
                concat!(
                    "uses print\n",
                    "fn boom(): Int {\n",
                    "    return mod(1, 0)\n",
                    "}\n",
                    "effect print(\"${boom()}\")\n",
                    "return 0\n",
                ),
                3,
            ),
        ];
        for (source, line) in cases {
            let result = execute(source, "test.hws", Granted::none());
            let fault = result
                .fault
                .unwrap_or_else(|| panic!("{source:?} should fault"));
            let diagnostic = fault.to_diagnostic();
            let at = diagnostic.location.as_ref().map(|l| l.line);
            assert_eq!(at, Some(line), "{source:?} reported {at:?}");
            assert_eq!(
                diagnostic.location.as_ref().map(|l| l.file.to_string()),
                Some("test.hws".to_owned())
            );
        }
    }

    /// A declaration whose target is a pattern works at the top level as it
    /// does inside a function: each name it binds is a top-level binding of
    /// its own, which a function of the same module can read.
    #[test]
    fn a_top_level_declaration_may_destructure() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "record Point = { x: Int, y: Int }\n",
                "constant point: Point = { x: 3, y: 4 }\n",
                "constant { x, y } = point\n",
                "constant [first, second] = [10, 20]\n",
                "fn sum(): Int {\n",
                "    return x + y + first + second\n",
                "}\n",
                "effect print(\"${x} ${y} ${first} ${second} ${sum()}\")\n",
                "return 0\n",
            )),
            ["3 4 10 20 37"]
        );
    }

    /// `sort(compare)` of section 14.1, including its own worked example
    /// and the stability the section requires: a comparator that places
    /// everything at zero keeps the input order.
    #[test]
    fn sort_with_a_comparator_follows_section_14_1() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "constant items = [\"bb\", \"a\", \"cc\", \"d\"]\n",
                "effect print(items.sort(fn(a: String, b: String): Int {\n",
                "    return a.length() - b.length()\n",
                "}).join(\",\"))\n",
                "constant same = [\"ba\", \"ab\", \"cc\", \"aa\"]\n",
                "effect print(same.sort(fn(a: String, b: String): Int { return 0 }).join(\",\"))\n",
                "constant down = [\"a\", \"c\", \"b\"]\n",
                "effect print(down.sort(fn(a: String, b: String): Int {\n",
                "    if a > b { return -1 }\n",
                "    if a < b { return 1 }\n",
                "    return 0\n",
                "}).join(\",\"))\n",
                "constant one = [\"x\"]\n",
                "effect print(one.sort(fn(a: String, b: String): Int { return 0 }).join(\",\"))\n",
                "constant empty: [String] = []\n",
                "effect print(\"${empty.sort(fn(a: String, b: String): Int { return 0 }).length()}\")\n",
                "return 0\n",
            )),
            ["a,d,bb,cc", "ba,ab,cc,aa", "c,b,a", "x", "0"]
        );
    }

    /// Section 14.1 defines `join` for a list of `String` alone.
    #[test]
    fn join_needs_a_list_of_strings() {
        let result = execute(
            "uses print\neffect print([1, 2].join(\",\"))\nreturn 0\n",
            "test.hws",
            Granted::none(),
        );
        assert!(!result.ok);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|d| d.code == hollowscript::Code::HS0330),
            "{:?}",
            result.diagnostics
        );
    }

    /// Chapter 15, end to end, against the specification's own worked
    /// examples for both sections.
    #[test]
    fn json_and_time_produce_the_specifications_own_output() {
        assert_eq!(
            printed(concat!(
                "uses print\n",
                "constant doc = Json.Array([Json.Int(1), Json.Object([\"a\": Json.Null])])\n",
                "effect print(json.pretty(doc, 2))\n",
                "match json.parse(\"{\\\"b\\\": [1, 2.5, true], \\\"a\\\": null}\") {\n",
                "    Ok(value) { effect print(json.stringify(value)) }\n",
                "    Err(e) { effect print(\"bad json at ${e.offset}: ${e.message}\") }\n",
                "}\n",
                "constant person = Json.Object([\"name\": Json.String(\"Ada\"), \"age\": Json.Int(36)])\n",
                "effect print(json.stringify(person))\n",
                "match json.field(person, \"name\") {\n",
                "    String(name) { effect print(\"hello ${name}\") }\n",
                "    else { effect print(\"no name\") }\n",
                "}\n",
                "match json.parse(\"{\") {\n",
                "    Ok(_) { effect print(\"parsed\") }\n",
                "    Err(e) { effect print(\"${e.message} at ${e.offset}\") }\n",
                "}\n",
                "match time.parse(\"2026-07-26T09:15:00+01:00\") {\n",
                "    Ok(instant) {\n",
                "        effect print(time.format(instant))\n",
                "        constant p = time.parts(instant)\n",
                "        effect print(\"${p.year} ${p.month} ${p.weekday}\")\n",
                "        match time.fromParts(p) {\n",
                "            Ok(back) { effect print(\"${back == instant}\") }\n",
                "            Err(m) { effect print(m) }\n",
                "        }\n",
                "    }\n",
                "    Err(m) { effect print(m) }\n",
                "}\n",
                "match time.parseDate(\"2026-13-45\") {\n",
                "    Ok(_) { effect print(\"valid\") }\n",
                "    Err(m) { effect print(m) }\n",
                "}\n",
                "match time.parseDate(\"2026-07-26\") {\n",
                "    Ok(d) { effect print(time.formatDate(d)) }\n",
                "    Err(m) { effect print(m) }\n",
                "}\n",
                "return 0\n",
            )),
            [
                // One `print` of a value written over several lines is one
                // entry of the output, not one per line.
                "[\n  1,\n  {\n    \"a\": null\n  }\n]",
                r#"{"a":null,"b":[1,2.5,true]}"#,
                r#"{"age":36,"name":"Ada"}"#,
                "hello Ada",
                "unexpected end of input at 1",
                "2026-07-26T08:15:00Z",
                "2026 7 7",
                "true",
                "invalid date",
                "2026-07-26",
            ]
        );
    }

    #[test]
    fn a_program_that_does_not_check_reports_diagnostics_and_never_runs() {
        let result = execute(
            "uses print\neffect print(3)\nreturn 0\n",
            "test.hws",
            Granted::none(),
        );
        assert!(!result.ok);
        assert!(result.output.is_empty(), "nothing ran, so nothing printed");
        assert!(
            result.diagnostics.iter().any(Diagnostic::is_error),
            "passing an Int where a String is required is a check error"
        );
    }

    /// A minimal, single-threaded HTTP/1.1 server for one test: binds to an
    /// ephemeral port on `127.0.0.1` and answers exactly `n` connections in
    /// order, each with whatever `handler` returns for it (the request's
    /// method, path, headers and body text, in that order). This, not the
    /// public internet, is what every `http` test below runs against.
    fn serve_n(
        n: usize,
        mut handler: impl FnMut(usize, &str, &str, &[(String, String)], &str) -> Vec<u8>
        + Send
        + 'static,
    ) -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{BufRead, BufReader, Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
        let port = listener.local_addr().expect("a local address").port();
        let handle = std::thread::spawn(move || {
            for i in 0..n {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let mut reader = BufReader::new(stream);
                let mut request_line = String::new();
                if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                    return;
                }
                let mut parts = request_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_owned();
                let path = parts.next().unwrap_or("").to_owned();
                let mut headers = Vec::new();
                let mut content_length = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    let line = line.trim_end_matches(['\r', '\n']);
                    if line.is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':') {
                        let name = name.trim().to_owned();
                        let value = value.trim().to_owned();
                        if name.eq_ignore_ascii_case("content-length") {
                            content_length = value.parse().unwrap_or(0);
                        }
                        headers.push((name, value));
                    }
                }
                let mut body_bytes = vec![0_u8; content_length];
                if content_length > 0 {
                    let _ = reader.read_exact(&mut body_bytes);
                }
                let body = String::from_utf8_lossy(&body_bytes).into_owned();
                let response = handler(i, &method, &path, &headers, &body);
                let mut stream = reader.into_inner();
                let _ = stream.write_all(&response);
                let _ = stream.flush();
            }
        });
        (port, handle)
    }

    /// Formats a raw HTTP/1.1 response: a status line, a fixed
    /// `Content-Length` and `Connection: close` (this server never reuses a
    /// connection), the caller's own headers, and the body.
    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> Vec<u8> {
        use std::fmt::Write as _;
        let mut text = format!("HTTP/1.1 {status} status\r\nConnection: close\r\n");
        for (name, value) in headers {
            let _ = writeln!(text, "{name}: {value}\r");
        }
        let _ = write!(text, "Content-Length: {}\r\n\r\n{body}", body.len());
        text.into_bytes()
    }

    /// A `Granted` with `http` allowed for `127.0.0.1:port` and short test
    /// limits, so a timeout or too-large test does not wait on the real
    /// 30 second, 8 MiB defaults.
    fn http_granted(port: u16) -> Granted {
        let mut granted = Granted::none();
        granted.http = vec![
            crate::http::parse_http_scope(&format!("127.0.0.1:{port}")).expect("a valid scope"),
        ];
        granted.http_limits = crate::http::HttpLimits {
            total_time: std::time::Duration::from_millis(500),
            max_body_bytes: 1024,
        };
        granted
    }

    #[test]
    fn http_get_reads_the_status_headers_and_body() {
        let (port, handle) = serve_n(1, |_, method, path, _, _| {
            assert_eq!(method, "GET");
            assert_eq!(path, "/status");
            response(200, &[("X-Test", "one"), ("X-Test", "two")], "hello")
        });
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.get(\"http://127.0.0.1:{port}/status\") {{\n",
                "    Ok(r) {{\n",
                "        effect print(\"status ${{r.status}}\")\n",
                "        effect print(\"body ${{r.body}}\")\n",
                "        constant found = r.headers.get(\"x-test\")\n",
                "        if found != none {{\n",
                "            effect print(\"header ${{found}}\")\n",
                "        }} else {{\n",
                "            effect print(\"header ?\")\n",
                "        }}\n",
                "    }}\n",
                "    Err(e) {{ effect print(\"failed\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port = port
        );
        let result = execute(&source, "test.hws", http_granted(port));
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(
            result.output,
            ["status 200", "body hello", "header one, two"]
        );
        handle.join().expect("the server thread does not panic");
    }

    #[test]
    fn http_denies_a_host_outside_the_grant() {
        let source = concat!(
            "uses print, http\n",
            "match effect http.get(\"http://198.51.100.1/\") {\n",
            "    Ok(r) { effect print(\"ok\") }\n",
            "    Err(Denied(m)) { effect print(\"denied\") }\n",
            "    Err(e) { effect print(\"other\") }\n",
            "}\n",
            "return 0\n",
        );
        // Grants a different host entirely, so nothing here ever connects.
        let mut granted = Granted::none();
        granted.http = vec![crate::http::parse_http_scope("example.com").expect("a valid scope")];
        let result = execute(source, "test.hws", granted);
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["denied"]);
    }

    #[test]
    fn http_follows_redirects_within_the_grant_and_adjusts_the_method() {
        let (port, handle) = serve_n(2, move |i, method, path, _, _| {
            if i == 0 {
                assert_eq!(method, "POST");
                assert_eq!(path, "/start");
                response(302, &[("Location", "/after")], "")
            } else {
                // Section 16.9: a 302 for a non-GET/HEAD request becomes a
                // GET without a body.
                assert_eq!(method, "GET");
                assert_eq!(path, "/after");
                response(200, &[], "landed")
            }
        });
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.request({{\n",
                "    method: \"POST\", url: \"http://127.0.0.1:{port}/start\",\n",
                "    headers: [:], body: \"payload\"\n",
                "}}) {{\n",
                "    Ok(r) {{ effect print(\"landed:${{r.body}}\") }}\n",
                "    Err(e) {{ effect print(\"failed\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port = port
        );
        let result = execute(&source, "test.hws", http_granted(port));
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["landed:landed"]);
        handle.join().expect("the server thread does not panic");
    }

    #[test]
    fn http_refuses_a_response_body_larger_than_the_limit() {
        let (port, handle) = serve_n(1, |_, _, _, _, _| response(200, &[], &"x".repeat(4096)));
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.get(\"http://127.0.0.1:{port}/big\") {{\n",
                "    Ok(r) {{ effect print(\"ok\") }}\n",
                "    Err(TooLarge(m)) {{ effect print(\"too large\") }}\n",
                "    Err(e) {{ effect print(\"other\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port = port
        );
        let result = execute(&source, "test.hws", http_granted(port));
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["too large"]);
        handle.join().expect("the server thread does not panic");
    }

    #[test]
    fn http_request_needs_an_allowed_method() {
        let source = concat!(
            "uses print, http\n",
            "match effect http.request({\n",
            "    method: \"FETCH\", url: \"http://127.0.0.1:1/x\",\n",
            "    headers: [:], body: none\n",
            "}) {\n",
            "    Ok(r) { effect print(\"ok\") }\n",
            "    Err(Invalid(m)) { effect print(\"invalid\") }\n",
            "    Err(e) { effect print(\"other\") }\n",
            "}\n",
            "return 0\n",
        );
        let mut granted = Granted::none();
        granted.http = vec![crate::http::parse_http_scope("127.0.0.1:1").expect("a valid scope")];
        let result = execute(source, "test.hws", granted);
        assert!(result.ok, "fault {:?}", result.fault);
        // Invalid is section 16.8/16.9's own order: checked, and refused,
        // before any connection is attempted.
        assert_eq!(result.output, ["invalid"]);
    }

    /// A program cannot set the `Host` header. Section 16.9: "for a
    /// host name the host... connects... with the granted name as the `Host`
    /// header". A program attempting `internal.admin.local` must still
    /// arrive at the server as the address and port it actually granted.
    #[test]
    fn http_a_program_cannot_set_the_host_header() {
        // A program-supplied `Host` header is refused (`Err(Invalid)`) before
        // any connection is attempted (`validate_headers` runs before the
        // URL is even resolved), so this deliberately targets a port nothing
        // listens on: were the fix wrong and the header allowed through, the
        // program would see `Err(Failed)` (connection refused) instead of
        // `Err(Invalid)`, not a silent, differently-wrong success.
        let source = concat!(
            "uses print, http\n",
            "match effect http.request({\n",
            "    method: \"GET\", url: \"http://127.0.0.1:1/a\",\n",
            "    headers: [\"Host\": \"internal.admin.local\"], body: none\n",
            "}) {\n",
            "    Ok(r) { effect print(\"ok\") }\n",
            "    Err(Invalid(m)) { effect print(\"invalid\") }\n",
            "    Err(e) { effect print(\"other\") }\n",
            "}\n",
            "return 0\n",
        );
        let mut granted = Granted::none();
        granted.http = vec![crate::http::parse_http_scope("127.0.0.1:1").expect("a valid scope")];
        let result = execute(source, "test.hws", granted);
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["invalid"]);
    }

    /// A program also cannot set
    /// `Content-Length`, `Transfer-Encoding` or `Connection` either, which is
    /// what a request-smuggling attempt through either of the first two
    /// needs.
    #[test]
    fn http_a_program_cannot_set_smuggling_headers() {
        for header in ["Content-Length", "Transfer-Encoding", "Connection"] {
            let source = format!(
                concat!(
                    "uses print, http\n",
                    "match effect http.request({{\n",
                    "    method: \"GET\", url: \"http://127.0.0.1:1/a\",\n",
                    "    headers: [\"{header}\": \"0\"], body: none\n",
                    "}}) {{\n",
                    "    Ok(r) {{ effect print(\"ok\") }}\n",
                    "    Err(Invalid(m)) {{ effect print(\"invalid\") }}\n",
                    "    Err(e) {{ effect print(\"other\") }}\n",
                    "}}\n",
                    "return 0\n",
                ),
                header = header
            );
            let mut granted = Granted::none();
            granted.http =
                vec![crate::http::parse_http_scope("127.0.0.1:1").expect("a valid scope")];
            let result = execute(&source, "test.hws", granted);
            assert!(result.ok, "fault {:?}", result.fault);
            assert_eq!(result.output, ["invalid"], "header {header}");
        }
    }

    /// Section 16.9: "a response with any status is returned as `Ok`";
    /// a 404 or a 500 is not `Err`.
    #[test]
    fn http_any_status_is_ok() {
        let (port, handle) = serve_n(2, |i, _, _, _, _| {
            if i == 0 {
                response(404, &[], "missing")
            } else {
                response(500, &[], "broken")
            }
        });
        let mut outputs = Vec::new();
        for path in ["/missing", "/broken"] {
            let source = format!(
                concat!(
                    "uses print, http\n",
                    "match effect http.get(\"http://127.0.0.1:{port}{path}\") {{\n",
                    "    Ok(r) {{ effect print(\"ok ${{r.status}}\") }}\n",
                    "    Err(e) {{ effect print(\"err\") }}\n",
                    "}}\n",
                    "return 0\n",
                ),
                port = port,
                path = path
            );
            let result = execute(&source, "test.hws", http_granted(port));
            assert!(result.ok, "fault {:?}", result.fault);
            outputs.extend(result.output);
        }
        assert_eq!(outputs, ["ok 404", "ok 500"]);
        handle.join().expect("the server thread does not panic");
    }

    /// An `Authorization` header is dropped before a redirect to a
    /// different origin is followed, even though the redirect target is
    /// still within the grant.
    #[test]
    fn http_drops_authorization_on_a_cross_origin_redirect() {
        let (port_b, handle_b) = serve_n(1, |_, _, _, headers, _| {
            let has_auth = headers
                .iter()
                .any(|(n, _)| n.eq_ignore_ascii_case("authorization"));
            response(200, &[], if has_auth { "leaked" } else { "clean" })
        });
        let (port_a, handle_a) = serve_n(1, move |_, _, _, _, _| {
            response(
                302,
                &[("Location", &format!("http://127.0.0.1:{port_b}/b"))],
                "",
            )
        });
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.request({{\n",
                "    method: \"GET\", url: \"http://127.0.0.1:{port_a}/a\",\n",
                "    headers: [\"Authorization\": \"Bearer secret\"], body: none\n",
                "}}) {{\n",
                "    Ok(r) {{ effect print(\"landed:${{r.body}}\") }}\n",
                "    Err(e) {{ effect print(\"failed\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port_a = port_a
        );
        let mut granted = Granted::none();
        granted.http = vec![
            crate::http::parse_http_scope(&format!("127.0.0.1:{port_a}")).expect("a valid scope"),
            crate::http::parse_http_scope(&format!("127.0.0.1:{port_b}")).expect("a valid scope"),
        ];
        granted.http_limits = crate::http::HttpLimits {
            total_time: std::time::Duration::from_millis(500),
            max_body_bytes: 1024,
        };
        let result = execute(&source, "test.hws", granted);
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["landed:clean"]);
        handle_a.join().expect("server a's thread does not panic");
        handle_b.join().expect("server b's thread does not panic");
    }

    /// A gzip stream that decodes to `1 + 258 * matches` copies of `byte`,
    /// built by hand (fixed Huffman codes: one literal, then `matches` copies
    /// of the longest match at distance one), so the test needs no
    /// compression library. A few dozen bytes decode to kilobytes.
    fn gzip_of_repeated(byte: u8, matches: usize) -> Vec<u8> {
        struct Bits {
            out: Vec<u8>,
            acc: u32,
            used: u32,
        }
        impl Bits {
            fn bit(&mut self, value: u32) {
                self.acc |= (value & 1) << self.used;
                self.used += 1;
                if self.used == 8 {
                    self.out.push(u8::try_from(self.acc).unwrap());
                    self.acc = 0;
                    self.used = 0;
                }
            }
            /// A value, least significant bit first (header fields).
            fn value(&mut self, value: u32, count: u32) {
                for i in 0..count {
                    self.bit(value >> i);
                }
            }
            /// A Huffman code, most significant bit first.
            fn code(&mut self, code: u32, len: u32) {
                for i in (0..len).rev() {
                    self.bit(code >> i);
                }
            }
        }
        let decoded_len = 1 + 258 * matches;
        let mut bits = Bits {
            out: Vec::new(),
            acc: 0,
            used: 0,
        };
        bits.value(1, 1); // the last block
        bits.value(1, 2); // fixed Huffman codes
        bits.code(0x30 + u32::from(byte), 8); // the literal
        for _ in 0..matches {
            bits.code(0xc5, 8); // length 258
            bits.code(0, 5); // distance 1
        }
        bits.code(0, 7); // end of block
        if bits.used > 0 {
            bits.out.push(u8::try_from(bits.acc).unwrap());
        }
        let mut crc = !0u32;
        for _ in 0..decoded_len {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        let mut gzip = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
        gzip.extend_from_slice(&bits.out);
        gzip.extend_from_slice(&(!crc).to_le_bytes());
        gzip.extend_from_slice(&u32::try_from(decoded_len).unwrap().to_le_bytes());
        gzip
    }

    /// A response with a binary body and a `Content-Encoding`.
    fn raw_response(status: u16, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        use std::fmt::Write as _;
        let mut head = format!("HTTP/1.1 {status} status\r\nConnection: close\r\n");
        for (name, value) in headers {
            let _ = writeln!(head, "{name}: {value}\r");
        }
        let _ = write!(head, "Content-Length: {}\r\n\r\n", body.len());
        let mut out = head.into_bytes();
        out.extend_from_slice(body);
        out
    }

    /// The body limit is on the decoded body. This server sends under a
    /// hundred bytes of gzip that decode to more than ten thousand, against a
    /// limit of 1,024: it used to be returned whole (300 KB of gzip decoded
    /// to 300 MiB).
    #[test]
    fn http_refuses_a_gzip_body_that_decodes_past_the_limit() {
        let body = gzip_of_repeated(b'a', 40);
        assert!(
            body.len() < 100,
            "the test's own bomb is small: {}",
            body.len()
        );
        let (port, handle) = serve_n(1, move |_, _, _, _, _| {
            raw_response(200, &[("Content-Encoding", "gzip")], &body)
        });
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.get(\"http://127.0.0.1:{port}/bomb\") {{\n",
                "    Ok(r) {{ effect print(\"ok ${{r.body.length()}}\") }}\n",
                "    Err(TooLarge(m)) {{ effect print(\"too large\") }}\n",
                "    Err(e) {{ effect print(\"other\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port = port
        );
        let result = execute(&source, "test.hws", http_granted(port));
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["too large"]);
        handle.join().expect("the server thread does not panic");
    }

    /// A gzip body that decodes to less than the limit still arrives, decoded.
    #[test]
    fn http_decodes_a_gzip_body_within_the_limit() {
        let body = gzip_of_repeated(b'z', 2); // 517 bytes
        let (port, handle) = serve_n(1, move |_, _, _, _, _| {
            raw_response(200, &[("Content-Encoding", "gzip")], &body)
        });
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.get(\"http://127.0.0.1:{port}/small\") {{\n",
                "    Ok(r) {{ effect print(\"ok ${{r.body.length()}}\") }}\n",
                "    Err(e) {{ effect print(\"err\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port = port
        );
        let result = execute(&source, "test.hws", http_granted(port));
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["ok 517"]);
        handle.join().expect("the server thread does not panic");
    }

    /// Every header a program supplied except the few that carry no
    /// credential is dropped on a redirect to another origin: not only
    /// `Authorization`, but `Cookie`, an API key and anything else.
    #[test]
    fn http_drops_every_credential_header_on_a_cross_origin_redirect() {
        let (port_b, handle_b) = serve_n(1, |_, _, _, headers, _| {
            let seen: Vec<String> = headers
                .iter()
                .map(|(n, _)| n.to_ascii_lowercase())
                .filter(|n| {
                    n.starts_with("x-") || n == "cookie" || n == "authorization" || n == "accept"
                })
                .collect();
            response(200, &[], &seen.join(","))
        });
        let (port_a, handle_a) = serve_n(1, move |_, _, _, _, _| {
            response(
                302,
                &[("Location", &format!("http://127.0.0.1:{port_b}/b"))],
                "",
            )
        });
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.request({{\n",
                "    method: \"GET\", url: \"http://127.0.0.1:{port_a}/a\",\n",
                "    headers: [\"Cookie\": \"session=1\", \"X-Api-Key\": \"k\", \"X-Custom\": \"c\",\n",
                "              \"Authorization\": \"Bearer s\", \"Accept\": \"text/plain\"], body: none\n",
                "}}) {{\n",
                "    Ok(r) {{ effect print(\"landed:${{r.body}}\") }}\n",
                "    Err(e) {{ effect print(\"failed\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port_a = port_a
        );
        let mut granted = Granted::none();
        granted.http = vec![
            crate::http::parse_http_scope(&format!("127.0.0.1:{port_a}")).expect("a valid scope"),
            crate::http::parse_http_scope(&format!("127.0.0.1:{port_b}")).expect("a valid scope"),
        ];
        granted.http_limits = crate::http::HttpLimits {
            total_time: std::time::Duration::from_millis(500),
            max_body_bytes: 1024,
        };
        let result = execute(&source, "test.hws", granted);
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(
            result.output,
            ["landed:accept"],
            "only the allowlisted header crosses to the other origin"
        );
        handle_a.join().expect("server a's thread does not panic");
        handle_b.join().expect("server b's thread does not panic");
    }

    /// A redirect to the same origin keeps every header, credentials included.
    #[test]
    fn http_keeps_headers_on_a_same_origin_redirect() {
        let (port, handle) = serve_n(2, move |i, _, _, headers, _| {
            if i == 0 {
                response(302, &[("Location", "/second")], "")
            } else {
                let has = |name: &str| headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name));
                response(200, &[], &format!("{}:{}", has("cookie"), has("x-api-key")))
            }
        });
        let source = format!(
            concat!(
                "uses print, http\n",
                "match effect http.request({{\n",
                "    method: \"GET\", url: \"http://127.0.0.1:{port}/first\",\n",
                "    headers: [\"Cookie\": \"a=b\", \"X-Api-Key\": \"k\"], body: none\n",
                "}}) {{\n",
                "    Ok(r) {{ effect print(r.body) }}\n",
                "    Err(e) {{ effect print(\"failed\") }}\n",
                "}}\n",
                "return 0\n",
            ),
            port = port
        );
        let result = execute(&source, "test.hws", http_granted(port));
        assert!(result.ok, "fault {:?}", result.fault);
        assert_eq!(result.output, ["true:true"]);
        handle.join().expect("the server thread does not panic");
    }

    /// Hop-by-hop and proxy headers cannot be set by a program.
    #[test]
    fn http_a_program_cannot_set_hop_by_hop_or_proxy_headers() {
        for header in [
            "Upgrade",
            "TE",
            "Expect",
            "Keep-Alive",
            "Proxy-Authorization",
            "Trailer",
            "X-Forwarded-Host",
            "X-Forwarded-For",
            "Forwarded",
            "X-HTTP-Method-Override",
        ] {
            let source = format!(
                concat!(
                    "uses print, http\n",
                    "match effect http.request({{\n",
                    "    method: \"GET\", url: \"http://127.0.0.1:1/a\",\n",
                    "    headers: [\"{header}\": \"x\"], body: none\n",
                    "}}) {{\n",
                    "    Ok(r) {{ effect print(\"ok\") }}\n",
                    "    Err(Invalid(m)) {{ effect print(\"invalid\") }}\n",
                    "    Err(e) {{ effect print(\"other\") }}\n",
                    "}}\n",
                    "return 0\n",
                ),
                header = header
            );
            let mut granted = Granted::none();
            granted.http =
                vec![crate::http::parse_http_scope("127.0.0.1:1").expect("a valid scope")];
            let result = execute(&source, "test.hws", granted);
            assert!(result.ok, "fault {:?}", result.fault);
            assert_eq!(result.output, ["invalid"], "header {header}");
        }
    }
}
