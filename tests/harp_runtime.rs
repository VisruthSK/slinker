use slinker::r_worker::protocol::{PROTOCOL_VERSION, TargetSpec, WorkerRequest, WorkerResponse};
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
        explicit.target_libraries[..2],
        [normalized(&first), normalized(&second)]
    );
    assert!(
        explicit
            .target_libraries
            .ends_with(&expected_default[expected_default.len().saturating_sub(1)..])
    );
    explicit.shutdown();
    std::fs::remove_dir_all(root).expect("remove explicit libraries");
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
    let executable = [
        r_home.join("bin").join("x64").join("R.exe"),
        r_home.join("bin").join("R.exe"),
        r_home.join("bin").join("R"),
    ]
    .into_iter()
    .find(|path| path.is_file())
    .expect("target R executable");
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
        .env("R_HOME", r_home)
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

fn normalized(path: &Path) -> PathBuf {
    PathBuf::from(path.to_string_lossy().replace('\\', "/"))
}

fn unique_temp(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "slinker-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}
