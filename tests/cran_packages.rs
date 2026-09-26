mod common;

use common::{
    assert_success, discover_r_home, install_package, install_package_using, run_r_output,
    run_r_with_site_profile, slinker,
};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn rebus_numbers_suite_passes_with_rebus_base_linked() {
    LinkedSuite {
        package: "rebus.numbers",
        linked: &["rebus.base"],
        checks: &[Check::Testthat],
    }
    .assert_passes();
}

#[test]
fn rslurm_suite_passes_with_whisker_linked() {
    LinkedSuite {
        package: "rslurm",
        linked: &["whisker"],
        checks: &[Check::Testthat],
    }
    .assert_passes();
}

#[test]
fn represtools_suite_passes_with_whisker_linked() {
    LinkedSuite {
        package: "represtools",
        linked: &["whisker"],
        checks: &[Check::Testthat],
    }
    .assert_passes();
}

#[test]
fn qrcode_suite_passes_with_assertthat_linked() {
    LinkedSuite {
        package: "qrcode",
        linked: &["assertthat"],
        checks: &[Check::Testthat],
    }
    .assert_passes();
}

#[test]
fn pkgcond_blocks_on_computed_scope_lookups() {
    LinkedSuite {
        package: "pkgcond",
        linked: &["assertthat"],
        checks: &[],
    }
    .assert_blocks(&[
        "DynamicLookup in pkgcond::find_scope: exists() looks up a name that is not a static string",
        "DynamicLookup in pkgcond::find_scope: get() looks up a name that is not a static string",
    ]);
}

#[test]
fn doubt_blocks_on_a_computed_lookup() {
    LinkedSuite {
        package: "doubt",
        linked: &["unglue"],
        checks: &[],
    }
    .assert_blocks(&[
        "DynamicLookup in doubt::?: get() looks up a name that is not a static string",
    ]);
}

#[test]
fn config_blocks_on_unaudited_yaml_native_code() {
    LinkedSuite {
        package: "config",
        linked: &["yaml"],
        checks: &[],
    }
    .assert_blocks(&[
        "UnknownNativeEffects in yaml: native component `yaml` has unanalyzed C-to-R callbacks",
    ]);
}

#[test]
fn here_suite_passes_with_rprojroot_linked() {
    LinkedSuite {
        package: "here",
        linked: &["rprojroot"],
        checks: &[
            Check::Testthat,
            Check::Script(
                r#"
                project <- normalizePath(file.path(tempdir(), "project"), winslash = "/", mustWork = FALSE)
                dir.create(file.path(project, "analysis"), recursive = TRUE)
                file.create(file.path(project, ".here"))
                writeLines("", file.path(project, "analysis", "report.R"))
                setwd(file.path(project, "analysis"))
                library(here)
                stopifnot(identical(normalizePath(here(), winslash = "/"), project))
                stopifnot(identical(here("data", "x.csv"), file.path(here(), "data", "x.csv")))
                stopifnot(grepl("contains a file '.here'", paste(capture.output(dr_here(), type = "message"), collapse = "\n"), fixed = TRUE))
                i_am("analysis/report.R")
                stopifnot(identical(normalizePath(here(), winslash = "/"), project))
                cat(basename(here()), here("a", "b") == file.path(here(), "a", "b"), "\n")
                "#,
            ),
        ],
    }
    .assert_passes();
}

#[test]
fn voucher_blocks_on_unproven_cli_and_fs_behavior() {
    LinkedSuite {
        package: "voucher",
        linked: &["cli", "fs"],
        checks: &[],
    }
    .assert_blocks(&[
        "DynamicLookup in cli::find_function_symbol: exists() looks up a name that is not a static string",
        "DynamicLookup in fs::register_s3_method: get() looks up a name that is not a static string",
        "DynamicLookup in voucher: dynamic system.file() package can name a Linked package",
        "asNamespace() with a dynamic namespace name",
        "getNamespaceVersion() with a dynamic package name can name a Linked package",
        "ObjectSystem in fs::compare.fs_path: NextMethod is not inside a registered method",
        "UnknownNativeEffects in cli: native component `cli` has unanalyzed C-to-R callbacks",
        "UnknownNativeEffects in fs: native component `fs` has unanalyzed C-to-R callbacks",
        "UnresolvedBinding in cli::",
    ]);
}

struct LinkedSuite<'a> {
    package: &'a str,
    linked: &'a [&'a str],
    checks: &'a [Check<'a>],
}

enum Check<'a> {
    Testthat,
    Script(&'a str),
}

#[derive(Clone, Copy, Debug)]
enum State {
    Absent,
    Installed,
    Loaded,
}

struct Provisioned {
    source: PathBuf,
    dependencies: PathBuf,
    runtime: PathBuf,
    testing: PathBuf,
    testing_needs_linked: bool,
    external: Vec<String>,
}

impl LinkedSuite<'_> {
    fn build(&self, provisioned: &Provisioned, output: &Path) -> std::process::Output {
        let external = provisioned.external.join(",");
        let mut arguments = vec![
            OsStr::new("build"),
            provisioned.source.as_os_str(),
            OsStr::new("--lib"),
            provisioned.dependencies.as_os_str(),
            OsStr::new("--output"),
            output.as_os_str(),
        ];
        if !external.is_empty() {
            arguments.extend([OsStr::new("--external"), OsStr::new(&external)]);
        }
        slinker(&arguments)
    }

    fn assert_blocks(&self, expected: &[&str]) {
        let r_home = discover_r_home();
        let provisioned = self.provision(&r_home);
        let work = tempfile::tempdir().expect("work directory");
        let output = work.path().join(self.package);
        let result = self.build(&provisioned, &output);
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(!result.status.success(), "{} built: {stderr}", self.package);
        assert!(stderr.contains("build preflight failed"), "{stderr}");
        assert!(
            !output.exists(),
            "a blocked build published {}",
            output.display()
        );
        for blocker in expected {
            assert!(
                stderr.contains(blocker),
                "missing blocker `{blocker}`:\n{stderr}"
            );
        }
    }

    fn assert_passes(&self) {
        let r_home = discover_r_home();
        let provisioned = self.provision(&r_home);
        let work = tempfile::tempdir().expect("work directory");
        let output = work.path().join(self.package);
        assert_success(
            &self.build(&provisioned, &output),
            &format!("slinker build {}", self.package),
        );

        let installed = work.path().join("installed");
        fs::create_dir(&installed).expect("installed library");
        install_package(&r_home, &output, &installed);
        let original = work.path().join("original");
        fs::create_dir(&original).expect("original library");
        let libraries = |state: State| {
            let mut libraries = vec![&installed, &provisioned.runtime];
            match state {
                State::Absent if provisioned.testing_needs_linked => {}
                State::Absent => libraries.push(&provisioned.testing),
                State::Installed | State::Loaded => {
                    libraries.extend([&provisioned.testing, &provisioned.dependencies]);
                }
            }
            std::env::join_paths(libraries).expect("runtime library path")
        };

        for check in self.checks {
            let states: &[State] = match check {
                Check::Testthat if provisioned.testing_needs_linked => {
                    &[State::Installed, State::Loaded]
                }
                Check::Testthat | Check::Script(_) => {
                    &[State::Absent, State::Installed, State::Loaded]
                }
            };
            let results = states
                .iter()
                .map(|&state| {
                    let summary = work.path().join(format!("summary-{state:?}.csv"));
                    let stdout = self.run_check(
                        &r_home,
                        libraries(state),
                        state,
                        check,
                        &self_tests(&provisioned.source),
                        &summary,
                    );
                    match check {
                        Check::Testthat => fs::read_to_string(&summary).expect("test summary"),
                        Check::Script(_) => stdout,
                    }
                })
                .collect::<Vec<_>>();
            for (state, result) in states.iter().zip(&results).skip(1) {
                assert_eq!(
                    &results[0], result,
                    "{} {state:?} differs from {:?}",
                    self.package, states[0]
                );
            }
            if let Check::Script(script) = check {
                if fs::read_dir(&original)
                    .expect("original library")
                    .next()
                    .is_none()
                {
                    install_package_using(
                        &r_home,
                        &provisioned.source,
                        &original,
                        std::env::join_paths([&provisioned.dependencies])
                            .expect("dependency library path"),
                    );
                }
                let paths = std::env::join_paths([
                    &original,
                    &provisioned.runtime,
                    &provisioned.testing,
                    &provisioned.dependencies,
                ])
                .expect("original library path");
                let output = run_r_output(&r_home, paths, script);
                assert_success(&output, &format!("original {} script", self.package));
                assert_eq!(
                    results[0],
                    String::from_utf8_lossy(&output.stdout),
                    "{} script differs from the original package",
                    self.package
                );
            }
        }
    }

    fn run_check(
        &self,
        r_home: &Path,
        libraries: OsString,
        state: State,
        check: &Check<'_>,
        tests: &Path,
        summary: &Path,
    ) -> String {
        let body = match check {
            Check::Testthat => format!(
                r#"
                Sys.setenv(NOT_CRAN = "true")
                results <- as.data.frame(testthat::test_dir(
                  {tests},
                  package = {package},
                  load_package = "installed",
                  stop_on_failure = TRUE,
                  reporter = "summary"
                ))
                write.csv(results[c("file", "test", "nb", "failed", "skipped", "error")], {summary}, row.names = FALSE)
                "#,
                tests = r_string(tests),
                package = r_string(self.package),
                summary = r_string(summary),
            ),
            Check::Script(script) => (*script).to_owned(),
        };
        let setup = match state {
            State::Absent => format!(
                "stopifnot(!nzchar(vapply({linked}, function(p) system.file(package = p), \"\")))",
                linked = r_vector(self.linked)
            ),
            State::Installed => format!(
                "stopifnot(nzchar(vapply({linked}, function(p) system.file(package = p), \"\")), !any(vapply({linked}, isNamespaceLoaded, TRUE)))",
                linked = r_vector(self.linked)
            ),
            State::Loaded => format!(
                "invisible(lapply({linked}, loadNamespace))",
                linked = r_vector(self.linked)
            ),
        };
        let output = run_r_output(r_home, libraries, &format!("{setup}\n{body}"));
        assert_success(
            &output,
            &format!("{} check with Linked {state:?}", self.package),
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn provision(&self, r_home: &Path) -> Provisioned {
        let cache = cache(self.package);
        let external = cache.join("external");
        let testing_needs_linked = cache.join("testing-needs-linked");
        let provisioned = Provisioned {
            source: cache.join("source").join(self.package),
            dependencies: cache.join("dependencies"),
            runtime: cache.join("runtime"),
            testing: cache.join("testing"),
            testing_needs_linked: false,
            external: Vec::new(),
        };
        run_r_with_site_profile(
            r_home,
            &format!(
                r#"
                package <- {package}
                linked <- {linked}
                if (identical(unname(getOption("repos")["CRAN"]), "@CRAN@")) {{
                  options(repos = c(CRAN = "https://cloud.r-project.org"))
                }}
                db <- available.packages()
                base <- c(rownames(installed.packages(priority = "base")), "R")
                fields <- c("Depends", "Imports", "LinkingTo")
                closure <- function(packages) {{
                  found <- tools::package_dependencies(packages, db = db, which = fields, recursive = TRUE)
                  setdiff(unique(c(packages, unlist(found))), base)
                }}
                installed <- function(library) rownames(installed.packages(library, noCache = TRUE))
                provide <- function(library, required, optional = character()) {{
                  dir.create(library, recursive = TRUE, showWarnings = FALSE)
                  .libPaths(c(library, .libPaths()))
                  missing <- setdiff(union(required, optional), installed(library))
                  if (length(missing)) install.packages(missing, lib = library, dependencies = FALSE)
                  absent <- setdiff(required, installed(library))
                  if (length(absent)) stop("could not install: ", toString(absent))
                }}
                hard <- setdiff(closure(package), package)
                stopifnot(all(linked %in% hard))
                external <- setdiff(hard, linked)
                runtime <- closure(external)
                overlap <- intersect(runtime, linked)
                if (length(overlap)) stop("the runtime needs Linked packages: ", toString(overlap))
                suggests <- intersect(tools::package_dependencies(package, db = db, which = "Suggests")[[1]], rownames(db))
                usable <- suggests[!vapply(
                  tools::package_dependencies(suggests, db = db, which = fields, recursive = TRUE),
                  function(dependencies) any(linked %in% dependencies),
                  logical(1)
                )]
                testing <- setdiff(closure("testthat"), runtime)
                provide({dependencies}, hard)
                provide({runtime_library}, runtime)
                provide({testing_library}, setdiff(testing, linked), setdiff(closure(usable), c(runtime, testing, linked)))
                if (!dir.exists({source})) {{
                  tarball <- download.packages(package, tempdir(), type = "source")[1, 2]
                  untar(tarball, exdir = dirname({source}))
                }}
                writeLines(external, {external})
                writeLines(as.character(any(linked %in% testing)), {testing_needs_linked})
                "#,
                package = r_string(self.package),
                linked = r_vector(self.linked),
                dependencies = r_string(&provisioned.dependencies),
                runtime_library = r_string(&provisioned.runtime),
                testing_library = r_string(&provisioned.testing),
                source = r_string(&provisioned.source),
                external = r_string(&external),
                testing_needs_linked = r_string(&testing_needs_linked),
            ),
        );
        Provisioned {
            external: fs::read_to_string(&external)
                .expect("External dependency list")
                .lines()
                .map(str::to_owned)
                .collect(),
            testing_needs_linked: fs::read_to_string(&testing_needs_linked)
                .expect("testing dependency marker")
                .trim()
                == "TRUE",
            ..provisioned
        }
    }
}

fn cache(package: &str) -> PathBuf {
    Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("cran")
        .join(package)
}

fn self_tests(source: &Path) -> PathBuf {
    source.join("tests").join("testthat")
}

fn r_string(value: impl AsRef<OsStr>) -> String {
    let value = value.as_ref().to_string_lossy().replace('\\', "/");
    format!("\"{}\"", value.replace('"', "\\\""))
}

fn r_vector(values: &[&str]) -> String {
    format!(
        "c({})",
        values.iter().map(r_string).collect::<Vec<_>>().join(", ")
    )
}
