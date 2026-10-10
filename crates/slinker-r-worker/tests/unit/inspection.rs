use crate::protocol::*;
use crate::runtime::WorkerRuntime;
use crate::scan::ObjectScanner;
use crate::serve::write_response;
use crate::*;
use harp::RFunctionExt;
use slinker_core::package::BindingName;
use slinker_core::package::BindingOrigin;
use slinker_core::package::BindingRepresentation;
use slinker_core::package::EnvironmentLabel;
use std::collections::{HashMap, HashSet};

#[test]
fn protocol_round_trips_binding_request() {
    let request = WorkerRequest::Binding {
        request_id: 7,
        package: PackageSpec {
            name: "fixture".into(),
            version: "1.0.0".into(),
            image_fingerprint: "image".into(),
            root: "/tmp/fixture".into(),
        },
        name: "foo".into(),
    };
    let json = serde_json::to_string(&request).unwrap();
    let decoded: WorkerRequest = serde_json::from_str(&json).unwrap();
    match decoded {
        WorkerRequest::Binding {
            request_id, name, ..
        } => {
            assert_eq!(request_id, 7);
            assert_eq!(name, "foo");
        }
        _ => panic!("wrong request variant"),
    }
}

#[test]
fn harp_inspection_preserves_lazy_active_altrep_and_private_state() {
    let r_home = test_r_home().expect("selected R installation");
    #[cfg(all(unix, not(target_os = "macos")))]
    if std::env::var_os("SLINKER_EMBEDDED_R_TEST").is_none() {
        let output =
            std::process::Command::new(std::env::current_exe().expect("current test executable"))
                .args([
                    "--exact",
                    "serve::tests::harp_inspection_preserves_lazy_active_altrep_and_private_state",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("SLINKER_EMBEDDED_R_TEST", "1")
                .env(
                    "LD_LIBRARY_PATH",
                    slinker_core::worker::target_library_path(&r_home)
                        .expect("target R library path"),
                )
                .output()
                .expect("run the test under the target R library path");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("test result: ok. 1 passed"),
            "embedded target R test failed:\n{stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (fixture_root, fixture_library, fixture_temp) =
        install_fixture(&r_home).expect("install worker fixture package");
    let target = TargetSpec {
        r_home,
        worker: 0,
        arch: match std::env::consts::ARCH {
            "x86" => "i386".into(),
            arch => arch.into(),
        },
        libraries: vec![fixture_library.clone()],
    };
    let mut runtime =
        WorkerRuntime::start(&target, super::Initialization(())).expect("start selected target R");
    let initial_target = runtime.target().expect("capture initialized target");
    assert_eq!(
        dunce::canonicalize(&initial_target.libraries[0]).expect("canonical first library"),
        dunce::canonicalize(&fixture_library).expect("canonical fixture library")
    );
    let package = PackageSpec {
        name: "harpfixture".into(),
        version: "1.0.0".into(),
        image_fingerprint: "fixture-image".into(),
        root: fixture_root,
    };
    let index = runtime
        .package_index(&package)
        .expect("index exact fixture root");
    assert!(index.on_load);
    assert!(index.binding_names.iter().any(|name| name == "good"));
    assert!(index.s3.iter().any(|registration| {
        registration.generic.name == "head"
            && registration.generic.package.as_deref() == Some("utils")
            && registration.method == "head.harpfixture"
    }));
    let on_load_ran = || {
        harp::parse_eval_base("isTRUE(getOption(\"harpfixture.onload\"))")
            .and_then(bool::try_from)
            .expect("query fixture load hook option")
    };
    assert!(!on_load_ran(), "indexing executed .onLoad");
    let good = runtime
        .binding(&package, "good")
        .expect("inspect demanded binding");
    assert!(good.binding.object.closure.is_some());
    assert!(!on_load_ran(), "binding inspection executed .onLoad");
    assert_eq!(
        runtime.target().expect("target after inspection").libraries,
        initial_target.libraries,
        "package inspection mutated .libPaths()"
    );
    let fixture = harp::parse_eval_global(
        r#"
        local({
          image <- new.env(parent = baseenv())
          private <- new.env(parent = baseenv())
          private$counter <- 0L
          makeActiveBinding("active", function() {
            private$counter <- private$counter + 1L
            1L
          }, private)
          delayedAssign("promise", {
            private$counter <- private$counter + 1L
            2L
          }, assign.env = private)
          private$self <- private
          private$handler <- function(expr) expr
          image$holder <- list(private = private, closure = function(x) x)
          class(image$holder) <- c("first_class", "second_class")
          image$counter <- 0L
          delayedAssign("unrelated", {
            image$counter <- image$counter + 1L
            99L
          }, assign.env = image)
          delayedAssign("lazy", function() 1L, assign.env = image)
          image$altrep <- 1:1000000
          list(image = image, private = private)
        })
        "#,
    )
    .expect("create worker fixture");
    let image = field(&fixture, "image").expect("image environment");
    let image_environment = harp::environment::Environment::new(image.clone());

    let lazy = harp::environment_iter::Binding::new(&image_environment, "lazy".into())
        .expect("lazy binding");
    let delivered = HashSet::new();
    let mut ids = HashMap::<libr::SEXP, EnvironmentLabel>::new();
    let mut identify = |environment: libr::SEXP| {
        let next = ids.len() + 1;
        Ok(ids
            .entry(environment)
            .or_insert_with(|| EnvironmentLabel::private(format!("fixture:{next}")))
            .clone())
    };
    let mut scanner = ObjectScanner::new(image.sexp, "fixture", &mut identify, &delivered);
    let lazy = scanner
        .top_binding("lazy", BindingOrigin::Code, lazy.value)
        .expect("inspect demanded promise");
    assert_eq!(
        lazy.object.representation,
        BindingRepresentation::LazyLoadPromise
    );
    assert!(lazy.object.closure.is_some());
    assert_eq!(
        i32::try_from(image_environment.get("counter").expect("image counter")).expect("integer"),
        0,
        "demanding lazy forced unrelated"
    );

    let holder = harp::environment_iter::Binding::new(&image_environment, "holder".into())
        .expect("holder binding");
    let holder = scanner
        .top_binding("holder", BindingOrigin::Code, holder.value)
        .expect("inspect retained private environment");
    assert_eq!(holder.object.embedded_closures.len(), 1);
    assert!(
        holder.object.embedded_closures[0]
            .source
            .contains("function")
    );
    assert_eq!(holder.object.classes, ["first_class", "second_class"]);
    let altrep = harp::environment_iter::Binding::new(&image_environment, "altrep".into())
        .expect("ALTREP binding");
    let altrep = scanner
        .top_binding("altrep", BindingOrigin::Code, altrep.value)
        .expect("classify ALTREP");
    assert!(matches!(
        altrep.object.representation,
        BindingRepresentation::Altrep { .. }
    ));
    assert!(
        altrep.object.issues.is_empty(),
        "base ALTREP serializes as a plain vector"
    );

    let scanned = scanner.finish();
    let private = scanned
        .private_environments
        .values()
        .find(|environment| environment.bindings.contains_key("active"))
        .expect("private environment");
    assert_eq!(
        private.bindings["active"].object.representation,
        BindingRepresentation::ActiveBinding
    );
    assert_eq!(
        private.bindings["promise"].object.representation,
        BindingRepresentation::Promise { forced: false }
    );
    assert_eq!(
        private.bindings["self"].object.environment.as_deref(),
        Some(private.id.as_str())
    );
    assert!(
        private.bindings["handler"]
            .object
            .closure
            .as_ref()
            .is_some_and(|closure| closure.source.starts_with("handler <- function"))
    );
    let mut later_scan = ObjectScanner::new(image.sexp, "fixture", &mut identify, &delivered);
    let rescanned = harp::environment_iter::Binding::new(&image_environment, "holder".into())
        .expect("holder binding");
    later_scan
        .top_binding("holder", BindingOrigin::Code, rescanned.value)
        .expect("inspect holder again");
    assert!(
        later_scan
            .finish()
            .private_environments
            .keys()
            .all(|label| scanned.private_environments.contains_key(label)),
        "an environment keeps its label across scans"
    );
    let private_environment = harp::environment::Environment::new(
        field(&fixture, "private").expect("private fixture environment"),
    );
    assert_eq!(
        i32::try_from(private_environment.get("counter").expect("counter")).expect("integer"),
        0
    );

    harp::parse_eval_global("cat('worker console noise')")
        .expect("write through the embedded R console");
    let response = WorkerResponse::SyntaxValidation {
        request_id: 19,
        accepted: true,
        message: None,
    };
    let mut protocol = Vec::new();
    write_response(&mut protocol, &response).expect("write isolated protocol response");
    assert!(matches!(
        serde_json::from_slice::<WorkerResponse>(&protocol).expect("decode protocol response"),
        WorkerResponse::SyntaxValidation {
            request_id: 19,
            accepted: true,
            ..
        }
    ));
    assert!(runtime.validate_syntax("function(").is_err());
    assert_eq!(
        runtime
            .target()
            .expect("recapture initialized target")
            .libraries,
        initial_target.libraries
    );
    payload_identity_stays_within_one_bundle(&mut runtime, &package, &fixture_temp);
    drop(runtime);
    std::fs::remove_dir_all(fixture_temp).expect("remove installed fixture");
}

fn payload_identity_stays_within_one_bundle(
    runtime: &mut WorkerRuntime,
    package: &PackageSpec,
    scratch: &std::path::Path,
) {
    let image = runtime.image_environment(package).expect("fixture image");
    harp::parse_eval_global(
        r#"
        .slinker_test_payloads <- function(image) {
          shared <- new.env(parent = emptyenv())
          cycle <- new.env(parent = emptyenv())
          cycle$self <- cycle
          parent <- new.env(parent = emptyenv())
          parent$tag <- "parent"
          counter <- local({
            count <- 0L
            function() {
              count <<- count + 1L
              count
            }
          })
          image$first <- list(state = shared, cycle = cycle, child = new.env(parent = parent))
          image$second <- structure(list(counter = counter), home = shared, class = "tagged")
          image$third <- counter
          image$external <- tools::file_ext
          stopifnot(
            identical(image$first$state, attr(image$second, "home")),
            !identical(
              unserialize(serialize(image$first, NULL))$state,
              attr(unserialize(serialize(image$second, NULL)), "home")
            )
          )
          .Internal(registerNamespace("root:harpfixture", image))
        }
        "#,
    )
    .expect("define payload fixture");
    harp::RFunction::new("", ".slinker_test_payloads")
        .add(image)
        .call()
        .expect("original identity is shared and separate serializations split it");
    let namespaces = [NamespaceImageSpec {
        package: package.clone(),
        registered_name: "root:harpfixture".into(),
    }];
    let payload = |names: &[&str]| PayloadSpec {
        package: package.clone(),
        names: names.iter().map(|name| BindingName::from(*name)).collect(),
        patches: Vec::new(),
    };

    let split = runtime
        .serialize_payloads(
            &namespaces,
            &[payload(&["first"]), payload(&["second", "third"])],
        )
        .expect("serialize split bundles");
    let PayloadSerialization::SharedIdentity { first, second } = split else {
        panic!("an environment shared by two bundles was serialized twice");
    };
    assert_eq!((first.payload, first.binding.as_str()), (0, "first"));
    assert_eq!((second.payload, second.binding.as_str()), (1, "second"));

    let independent = runtime
        .serialize_payloads(
            &namespaces,
            &[payload(&["first"]), payload(&["external", "good"])],
        )
        .expect("serialize independent bundles");
    let PayloadSerialization::Serialized { bundles } = independent else {
        panic!("independent bundles were reported as sharing identity");
    };
    assert!(bundles[0].namespaces.is_empty());
    let mut observed = bundles[1].namespaces.clone();
    observed.sort();
    assert_eq!(observed, ["root:harpfixture", "tools"]);

    let joint = runtime
        .serialize_payloads(&namespaces, &[payload(&["first", "second", "third"])])
        .expect("serialize one bundle");
    let PayloadSerialization::Serialized { bundles } = joint else {
        panic!("bindings of one bundle were reported as sharing identity across bundles");
    };
    let restored = scratch.join("joint-bundle");
    std::fs::write(&restored, &bundles[0].bytes).expect("write serialized bundle");
    harp::parse_eval_global(&format!(
        r#"
        local({{
          path <- "{}"
          restored <- unserialize(readBin(path, "raw", file.size(path)))
          stopifnot(
            identical(restored$first$state, attr(restored$second, "home")),
            identical(restored$first$cycle$self, restored$first$cycle),
            identical(get("tag", envir = restored$first$child), "parent"),
            identical(environment(restored$second$counter), environment(restored$third)),
            identical(get("image", envir = environment(restored$third)),
              .Internal(getRegisteredNamespace("root:harpfixture"))),
            identical(restored$third(), 1L),
            identical(restored$second$counter(), 2L),
            inherits(restored$second, "tagged")
          )
          .Internal(unregisterNamespace("root:harpfixture"))
        }})
        "#,
        restored.to_string_lossy().replace('\\', "/")
    ))
    .expect("one bundle preserves sharing, cycles, parents, enclosures, and attributes");
}

fn test_r_home() -> Option<std::path::PathBuf> {
    if let Some(home) = std::env::var_os("R_HOME") {
        return dunce::canonicalize(home).ok();
    }
    let output = if cfg!(windows) {
        std::process::Command::new("cmd")
            .args(["/c", "R RHOME"])
            .output()
            .ok()?
    } else {
        std::process::Command::new("R").arg("RHOME").output().ok()?
    };
    let home = String::from_utf8(output.stdout).ok()?;
    dunce::canonicalize(
        home.lines()
            .rev()
            .find(|line| !line.trim().is_empty())?
            .trim(),
    )
    .ok()
}

fn install_fixture(
    r_home: &std::path::Path,
) -> Option<(std::path::PathBuf, std::path::PathBuf, std::path::PathBuf)> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = std::env::temp_dir().join(format!(
        "slinker-harp-fixture-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let source = temp.join("source");
    let library = temp.join("library");
    std::fs::create_dir_all(source.join("R")).ok()?;
    std::fs::create_dir_all(&library).ok()?;
    std::fs::write(
        source.join("DESCRIPTION"),
        "Package: harpfixture\nVersion: 1.0.0\nTitle: Harp fixture\nDescription: Harp worker fixture.\nAuthors@R: person('A', 'B', email='a@example.com', role=c('aut','cre'))\nLicense: MIT\nEncoding: UTF-8\n",
    )
    .ok()?;
    std::fs::write(
        source.join("NAMESPACE"),
        "export(good)\nS3method(utils::head, harpfixture)\n",
    )
    .ok()?;
    std::fs::write(
        source.join("R").join("fixture.R"),
        r#"
good <- function() 1L
head.harpfixture <- function(x, ...) x
unrelated <- function() stop("unrelated binding executed")
.onLoad <- function(...) options(harpfixture.onload = TRUE)
"#,
    )
    .ok()?;
    let executable = slinker_core::r_executable(r_home)?;
    let status = std::process::Command::new(executable)
        .args(["CMD", "INSTALL", "--no-test-load"])
        .arg(format!("--library={}", library.display()))
        .arg(&source)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    Some((library.join("harpfixture"), library, temp))
}
