mod common;

use common::{
    assert_success, discover_r_home, install_package_using, run_r_output, run_r_with_site_profile,
    slinker,
};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn rebus_numbers_blocks_on_an_unproven_callable() {
    LinkedSuite {
        package: "rebus.numbers",
        linked: &["rebus.base"],
        checks: &[Check::Testthat],
    }
    .assert_blocks(&["DynamicLookup in rebus.numbers::number_range: do.call()"]);
}

#[test]
fn rslurm_blocks_on_unproven_callables() {
    LinkedSuite {
        package: "rslurm",
        linked: &["whisker"],
        checks: &[Check::Testthat],
    }
    .assert_blocks(&[
        "DynamicLookup in rslurm::get_slurm_out: do.call()",
        "DynamicLookup in whisker::renderTemplate: do.call()",
    ]);
}

#[test]
fn represtools_blocks_on_unproven_callables() {
    LinkedSuite {
        package: "represtools",
        linked: &["whisker"],
        checks: &[Check::Testthat],
    }
    .assert_blocks(&[
        "DynamicLookup in represtools::Analyze: do.call()",
        "DynamicLookup in represtools::Cook: do.call()",
        "DynamicLookup in represtools::Gather: do.call()",
        "DynamicLookup in represtools::Present: do.call()",
        "DynamicLookup in whisker::renderTemplate: do.call()",
    ]);
}

#[test]
fn qrcode_blocks_on_unselected_optional_package_availability() {
    LinkedSuite {
        package: "qrcode",
        linked: &["assertthat"],
        checks: &[],
    }
    .assert_blocks(&[
        "OptionalAvailability in qrcode::generate_svg.qr_logo: reachable code depends on whether unselected optional package `knitr` is installed",
        "OptionalAvailability in qrcode::read_logo: reachable code depends on whether unselected optional package `png` is installed",
        "OptionalAvailability in qrcode::validate_qr: reachable code depends on whether unselected optional package `httr` is installed",
    ]);
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
fn here_blocks_on_an_unproven_rprojroot_callable() {
    LinkedSuite {
        package: "here",
        linked: &["rprojroot"],
        checks: &[
            Check::Testthat,
            Check::Script(
                r#"
                dir.create(file.path(tempdir(), "project", "analysis"), recursive = TRUE)
                project <- normalizePath(file.path(tempdir(), "project"), winslash = "/")
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
    .assert_blocks(&["DynamicLookup in rprojroot::path: do.call()"]);
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
        "asNamespace() with an unproven namespace name",
        "getNamespaceVersion() with a dynamic package name can name a Linked package",
        "UnknownNativeEffects in cli: native component `cli` has unanalyzed C-to-R callbacks",
        "UnknownNativeEffects in fs: native component `fs` has unanalyzed C-to-R callbacks",
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
    Installed,
    Loaded,
}

struct Provisioned {
    source: PathBuf,
    dependencies: PathBuf,
    runtime: PathBuf,
    testing: PathBuf,
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
        self.assert_original(&r_home, &provisioned);
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

    fn assert_original(&self, r_home: &Path, provisioned: &Provisioned) {
        if self.checks.is_empty() {
            return;
        }
        let work = tempfile::tempdir().expect("original oracle directory");
        let original = work.path().join("library");
        fs::create_dir(&original).expect("original library");
        install_package_using(
            r_home,
            &provisioned.source,
            &original,
            provisioned.dependencies.as_os_str(),
        );
        let libraries = std::env::join_paths([
            &original,
            &provisioned.runtime,
            &provisioned.testing,
            &provisioned.dependencies,
        ])
        .expect("oracle library path");
        for check in self.checks {
            let mut results = Vec::new();
            for state in [State::Installed, State::Loaded] {
                let summary = work.path().join(format!("summary-{state:?}.csv"));
                let stdout = self.run_check(
                    r_home,
                    libraries.clone(),
                    state,
                    check,
                    &self_tests(&provisioned.source),
                    &summary,
                );
                results.push(match check {
                    Check::Testthat => fs::read_to_string(summary).expect("test summary"),
                    Check::Script(_) => stdout,
                });
            }
            assert_eq!(
                results[0], results[1],
                "{} original behavior depends on preloading",
                self.package
            );
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
        let provisioned = Provisioned {
            source: cache.join("source").join(self.package),
            dependencies: cache.join("dependencies"),
            runtime: cache.join("runtime"),
            testing: cache.join("testing"),
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
                if (!file.exists(file.path({source}, "DESCRIPTION"))) {{
                  tarball <- download.packages(package, tempdir(), type = "source")[1, 2]
                  untar(tarball, exdir = dirname({source}))
                }}
                writeLines(external, {external})
                "#,
                package = r_string(self.package),
                linked = r_vector(self.linked),
                dependencies = r_string(&provisioned.dependencies),
                runtime_library = r_string(&provisioned.runtime),
                testing_library = r_string(&provisioned.testing),
                source = r_string(&provisioned.source),
                external = r_string(&external),
            ),
        );
        Provisioned {
            external: fs::read_to_string(&external)
                .expect("External dependency list")
                .lines()
                .map(str::to_owned)
                .collect(),
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
