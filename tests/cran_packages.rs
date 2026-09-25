mod common;

use common::{
    assert_success, discover_r_home, install_package, run_r, run_r_with_site_profile, slinker,
};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn rebus_numbers_suite_passes_with_rebus_base_linked() {
    LinkedSuite {
        package: "rebus.numbers",
        linked: &["rebus.base"],
    }
    .assert_passes();
}

#[test]
fn rslurm_suite_passes_with_whisker_linked() {
    LinkedSuite {
        package: "rslurm",
        linked: &["whisker"],
    }
    .assert_passes();
}

#[test]
fn represtools_suite_passes_with_whisker_linked() {
    LinkedSuite {
        package: "represtools",
        linked: &["whisker"],
    }
    .assert_passes();
}

#[test]
fn qrcode_suite_passes_with_assertthat_linked() {
    LinkedSuite {
        package: "qrcode",
        linked: &["assertthat"],
    }
    .assert_passes();
}

#[test]
fn pkgcond_suite_passes_with_assertthat_linked() {
    LinkedSuite {
        package: "pkgcond",
        linked: &["assertthat"],
    }
    .assert_passes();
}

#[test]
fn doubt_suite_passes_with_unglue_linked() {
    LinkedSuite {
        package: "doubt",
        linked: &["unglue"],
    }
    .assert_passes();
}

struct LinkedSuite<'a> {
    package: &'a str,
    linked: &'a [&'a str],
}

struct Provisioned {
    source: PathBuf,
    dependencies: PathBuf,
    runtime: PathBuf,
    external: Vec<String>,
}

impl LinkedSuite<'_> {
    fn assert_passes(&self) {
        let r_home = discover_r_home();
        let provisioned = self.provision(&r_home);
        let work = tempfile::tempdir().expect("work directory");
        let output = work.path().join(self.package);
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
        assert_success(
            &slinker(&arguments),
            &format!("slinker build {}", self.package),
        );

        let installed = work.path().join("installed");
        fs::create_dir(&installed).expect("installed library");
        install_package(&r_home, &output, &installed);
        let libraries =
            std::env::join_paths([&installed, &provisioned.runtime]).expect("runtime library path");
        run_r(
            &r_home,
            libraries,
            &format!(
                r#"
                linked <- {linked}
                stopifnot(!nzchar(vapply(linked, function(p) system.file(package = p), "")))
                Sys.setenv(NOT_CRAN = "true")
                testthat::test_dir(
                  {tests},
                  package = {package},
                  load_package = "installed",
                  stop_on_failure = TRUE
                )
                "#,
                linked = r_vector(self.linked),
                tests = r_string(provisioned.source.join("tests").join("testthat")),
                package = r_string(self.package),
            ),
        );
    }

    fn provision(&self, r_home: &Path) -> Provisioned {
        let cache = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("cran")
            .join(self.package);
        let provisioned = Provisioned {
            source: cache.join("source").join(self.package),
            dependencies: cache.join("dependencies"),
            runtime: cache.join("runtime"),
            external: Vec::new(),
        };
        let external = cache.join("external");
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
                provide <- function(library, packages) {{
                  dir.create(library, recursive = TRUE, showWarnings = FALSE)
                  missing <- setdiff(packages, rownames(installed.packages(library, noCache = TRUE)))
                  .libPaths(c(library, .libPaths()))
                  if (length(missing)) install.packages(missing, lib = library, dependencies = FALSE, quiet = TRUE)
                  stopifnot(all(packages %in% rownames(installed.packages(library, noCache = TRUE))))
                }}
                hard <- setdiff(closure(package), package)
                stopifnot(all(linked %in% hard))
                external <- setdiff(hard, linked)
                suggests <- tools::package_dependencies(package, db = db, which = "Suggests")[[1]]
                runtime <- closure(c(external, intersect(suggests, rownames(db)), "testthat"))
                overlap <- intersect(runtime, linked)
                if (length(overlap)) stop("the test runtime needs Linked packages: ", toString(overlap))
                provide({dependencies}, hard)
                provide({runtime_library}, runtime)
                if (!dir.exists({source})) {{
                  tarball <- download.packages(package, tempdir(), type = "source")[1, 2]
                  untar(tarball, exdir = dirname({source}))
                }}
                writeLines(external, {external})
                "#,
                package = r_string(self.package),
                linked = r_vector(self.linked),
                dependencies = r_string(&provisioned.dependencies),
                runtime_library = r_string(&provisioned.runtime),
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
