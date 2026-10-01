use slinker::r_worker::protocol::{
    AppendedArgumentSpec, PROTOCOL_VERSION, RelocationSiteSpec, TargetSpec, WorkerRequest,
    WorkerResponse,
};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

#[test]
fn worker_library_selection_matches_target_r_semantics() {
    let r_home = discover_r_home().expect("selected R installation");
    let expected_default = target_r_libraries(&r_home);
    let default = WorkerProbe::start(&r_home, Vec::new()).expect("default worker");
    assert_eq!(default.target_libraries, expected_default);
    default.shutdown();

    let root = unique_temp("libraries");
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&first).expect("first explicit library");
    std::fs::create_dir_all(&second).expect("second explicit library");
    let explicit =
        WorkerProbe::start(&r_home, vec![first.clone(), second.clone()]).expect("explicit worker");
    assert_eq!(
        explicit.target_libraries[..2]
            .iter()
            .map(|library| canonical(library))
            .collect::<Vec<_>>(),
        [canonical(&first), canonical(&second)]
    );
    assert!(
        explicit
            .target_libraries
            .ends_with(&expected_default[expected_default.len().saturating_sub(1)..])
    );
    explicit.shutdown();
    std::fs::remove_dir_all(root).expect("remove explicit libraries");
}

#[test]
fn relocation_verifier_accepts_exactly_the_planned_replacements() {
    let r_home = discover_r_home().expect("selected R installation");
    let mut worker = WorkerProbe::start(&r_home, Vec::new()).expect("worker");

    let original = "f <- function(x) {\n  # keep going\n  y <- pkg::g(h(x),   pkg::k)   \n  if (requireNamespace(\"pkg\", quietly = TRUE)) y else utils::packageDescription(\"pkg\", fields = \"Version\")\n}";
    let sites = vec![
        site(
            original,
            "pkg::g",
            "base::get(\"g\", envir = base::asNamespace(\"root\"), inherits = FALSE)",
        ),
        site(original, "pkg::k", "base::getExportedValue(\"pkg\", \"k\")"),
        site(
            original,
            "requireNamespace(\"pkg\", quietly = TRUE)",
            "TRUE",
        ),
        appending(
            site_in(original, "\"pkg\", fields", "\"pkg\"", "\"pkg\""),
            "lib.loc",
            "base::system.file(\"slinker\", \"resources\", package = \"root\")",
        ),
    ];
    let rewritten = splice(original, &sites);
    assert_eq!(worker.verify(original, &rewritten, &sites), Ok(()));
    let reformatted = rewritten
        .replace("  # keep going\n", "")
        .replace("   ", " ")
        .replace(") y else", ")\n    y\n  else");
    assert_ne!(reformatted, rewritten);
    assert_eq!(worker.verify(original, &reformatted, &sites), Ok(()));

    let unchanged = "f <- function(x)   x + 1L  # trailing";
    let reformatted = worker.normalize(unchanged);
    assert_ne!(reformatted, unchanged);
    assert_eq!(worker.verify(unchanged, &reformatted, &[]), Ok(()));
    worker.shutdown();
}

#[test]
fn relocation_verifier_rejects_syntactically_valid_unplanned_changes() {
    let r_home = discover_r_home().expect("selected R installation");
    let mut worker = WorkerProbe::start(&r_home, Vec::new()).expect("worker");

    let original = "f <- function(x) pkg::g(x) + 1";
    let sites = [site(
        original,
        "pkg::g",
        "base::getExportedValue(\"pkg\", \"g\")",
    )];
    reject(
        &mut worker,
        original,
        &splice(original, &sites).replace("+ 1", "+ 2"),
        &sites,
        UNPLANNED,
    );

    let original = "f <- function(x) -\"pkg\" %in% x";
    let sites = [site(original, "\"pkg\" %in% x", "TRUE")];
    reject(
        &mut worker,
        original,
        &splice(original, &sites),
        &sites,
        NOT_A_SITE,
    );

    let original = "f <- function(x) -x^2";
    let sites = [site_in(original, "x^", "x", "a + b")];
    reject(
        &mut worker,
        original,
        &splice(original, &sites),
        &sites,
        UNPLANNED,
    );

    let original = "f <- function() \"requireNamespace('pkg')\"";
    let sites = [site(original, "requireNamespace('pkg')", "TRUE")];
    reject(
        &mut worker,
        original,
        &splice(original, &sites),
        &sites,
        NOT_A_SITE,
    );

    let original = "f <- function(x) pkg::g(x)";
    let sites = [site(original, "pkg::g", "base::get(\"g\", envir = e)")];
    let deparsed = worker.normalize(&splice(original, &sites));
    assert!(deparsed.contains("(base::get(\"g\", envir = e))(x)"));
    reject(&mut worker, original, &deparsed, &sites, UNPLANNED);

    reject(
        &mut worker,
        "f <- function(x) x + 1",
        "f <- function(x) x + 2",
        &[],
        UNPLANNED,
    );
    worker.shutdown();
}

#[test]
fn relocation_verifier_rejects_a_malformed_rewrite() {
    let r_home = discover_r_home().expect("selected R installation");
    let mut worker = WorkerProbe::start(&r_home, Vec::new()).expect("worker");
    let original = "f <- function() x <- \"pkg\"";
    let sites = [appending(
        site(original, "\"pkg\"", "\"pkg\""),
        "lib.loc",
        "NULL",
    )];
    let rewritten = splice(original, &sites);
    assert!(!worker.parses(&rewritten));
    assert!(worker.verify(original, &rewritten, &sites).is_err());
    worker.shutdown();
}

const UNPLANNED: &str = "differs from the original code with its planned replacements";
const NOT_A_SITE: &str = "whole expression";

fn reject(
    worker: &mut WorkerProbe,
    original: &str,
    rewritten: &str,
    sites: &[RelocationSiteSpec],
    reason: &str,
) {
    assert!(
        worker.parses(rewritten),
        "target R parses the rewrite: {rewritten}"
    );
    let rejection = worker
        .verify(original, rewritten, sites)
        .expect_err("unplanned rewrite accepted");
    assert!(
        rejection.contains(reason),
        "rejected for another reason than `{reason}`: {rejection}"
    );
}

fn site(source: &str, needle: &str, replacement: &str) -> RelocationSiteSpec {
    site_in(source, needle, needle, replacement)
}

fn site_in(source: &str, context: &str, needle: &str, replacement: &str) -> RelocationSiteSpec {
    let start = source.find(context).expect("context in source")
        + context.find(needle).expect("site in context");
    RelocationSiteSpec {
        start,
        end: start + needle.len(),
        replacement: replacement.into(),
        appended_argument: None,
    }
}

fn appending(site: RelocationSiteSpec, name: &str, value: &str) -> RelocationSiteSpec {
    RelocationSiteSpec {
        appended_argument: Some(AppendedArgumentSpec {
            name: name.into(),
            value: value.into(),
        }),
        ..site
    }
}

fn splice(original: &str, sites: &[RelocationSiteSpec]) -> String {
    let mut ordered = sites.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|site| std::cmp::Reverse(site.start));
    let mut source = original.to_owned();
    for site in ordered {
        let text = match &site.appended_argument {
            None => site.replacement.clone(),
            Some(argument) => format!(
                "{}, {} = {}",
                site.replacement, argument.name, argument.value
            ),
        };
        source.replace_range(site.start..site.end, &text);
    }
    source
}

struct WorkerProbe {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<File>,
    protocol_path: PathBuf,
    target_libraries: Vec<PathBuf>,
}

impl WorkerProbe {
    fn start(r_home: &Path, libraries: Vec<PathBuf>) -> Result<Self, String> {
        let protocol_path = unique_temp("protocol").with_extension("jsonl");
        let protocol_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&protocol_path)
            .map_err(|error| error.to_string())?;
        let mut child = Command::new(env!("CARGO_BIN_EXE_slinker"))
            .arg("__r-worker")
            .arg(&protocol_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?;
        let input = child.stdin.take().ok_or("worker stdin")?;
        let mut probe = Self {
            child,
            input: BufWriter::new(input),
            output: BufReader::new(protocol_file),
            protocol_path,
            target_libraries: Vec::new(),
        };
        let response = probe.exchange(&WorkerRequest::Hello {
            protocol: PROTOCOL_VERSION,
            target: TargetSpec {
                r_home: r_home.to_path_buf(),
                arch: match std::env::consts::ARCH {
                    "x86" => "i386".into(),
                    arch => arch.into(),
                },
                worker: 1,
                libraries,
            },
        })?;
        match response {
            WorkerResponse::Hello { target, .. } => {
                probe.target_libraries = target.libraries;
                Ok(probe)
            }
            response => Err(format!("unexpected hello response: {response:?}")),
        }
    }

    fn exchange(&mut self, request: &WorkerRequest) -> Result<WorkerResponse, String> {
        serde_json::to_writer(&mut self.input, request).map_err(|error| error.to_string())?;
        self.input
            .write_all(b"\n")
            .map_err(|error| error.to_string())?;
        self.input.flush().map_err(|error| error.to_string())?;
        let mut line = String::new();
        loop {
            let read = self
                .output
                .read_line(&mut line)
                .map_err(|error| error.to_string())?;
            if read != 0 && line.ends_with('\n') {
                return serde_json::from_str(&line).map_err(|error| error.to_string());
            }
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!("worker exited before response: {status}"));
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    fn verify(
        &mut self,
        original: &str,
        rewritten: &str,
        sites: &[RelocationSiteSpec],
    ) -> Result<(), String> {
        let request_id = next_request();
        match self
            .exchange(&WorkerRequest::VerifyRelocation {
                request_id,
                original: original.into(),
                rewritten: rewritten.into(),
                sites: sites.to_vec(),
            })
            .expect("relocation verification response")
        {
            WorkerResponse::SyntaxValidation {
                request_id: response,
                accepted: true,
                ..
            } if response == request_id => Ok(()),
            WorkerResponse::SyntaxValidation {
                request_id: response,
                accepted: false,
                message,
            } if response == request_id => Err(message.expect("rejection message")),
            response => panic!("unexpected relocation verification response: {response:?}"),
        }
    }

    fn parses(&mut self, source: &str) -> bool {
        let request_id = next_request();
        match self
            .exchange(&WorkerRequest::ValidateSyntax {
                request_id,
                source: source.into(),
            })
            .expect("syntax validation response")
        {
            WorkerResponse::SyntaxValidation {
                request_id: response,
                accepted,
                ..
            } if response == request_id => accepted,
            response => panic!("unexpected syntax validation response: {response:?}"),
        }
    }

    fn normalize(&mut self, source: &str) -> String {
        let request_id = next_request();
        match self
            .exchange(&WorkerRequest::NormalizeSyntax {
                request_id,
                source: source.into(),
            })
            .expect("syntax normalization response")
        {
            WorkerResponse::NormalizedSyntax {
                request_id: response,
                source,
                ..
            } if response == request_id => source,
            response => panic!("unexpected syntax normalization response: {response:?}"),
        }
    }

    fn shutdown(mut self) {
        let _ = self.exchange(&WorkerRequest::Shutdown);
        let _ = self.child.wait();
        let _ = std::fs::remove_file(self.protocol_path);
    }
}

fn discover_r_home() -> Option<PathBuf> {
    let output = if cfg!(windows) {
        Command::new("cmd").args(["/c", "R RHOME"]).output().ok()
    } else {
        Command::new("R").arg("RHOME").output().ok()
    };
    let discovered = output
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .and_then(|output| {
            output
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .map(str::trim)
                .map(PathBuf::from)
        })
        .or_else(|| std::env::var_os("R_HOME").map(PathBuf::from))?;
    dunce::canonicalize(discovered).ok()
}

fn target_r_libraries(r_home: &Path) -> Vec<PathBuf> {
    let executable = slinker::r_executable(r_home).expect("target R executable");
    let missing_user = r_home.join("etc").join("__slinker_no_user_Renviron__");
    let output = Command::new(executable)
        .args([
            "--slave",
            "--no-save",
            "--no-restore",
            "--no-site-file",
            "--no-init-file",
            "-e",
            "cat(.libPaths(), sep='\\n')",
        ])
        .env("R_ENVIRON_USER", missing_user)
        .env_remove("R_LIBS")
        .env_remove("R_LIBS_USER")
        .env_remove("R_LIBS_SITE")
        .output()
        .expect("query target R libraries");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("UTF-8 library output")
        .lines()
        .filter(|line| !line.is_empty())
        .map(PathBuf::from)
        .collect()
}

fn canonical(path: &Path) -> PathBuf {
    dunce::canonicalize(path).expect("canonical library path")
}

fn unique_temp(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "slinker-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

fn next_request() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}
