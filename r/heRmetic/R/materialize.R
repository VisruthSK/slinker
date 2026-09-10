hrm_materialize <- function(spec_path) {
  spec_env <- new.env(parent = baseenv())
  sys.source(spec_path, envir = spec_env)
  spec <- spec_env$spec
  if (is.null(spec)) stop("materialization spec did not define 'spec'", call. = FALSE)

  packages <- spec$packages
  package_names <- vapply(packages, `[[`, character(1L), "name")
  if (anyDuplicated(package_names)) stop("duplicate synthetic package", call. = FALSE)

  provided <- spec$provided
  provided_names <- if (length(provided)) vapply(provided, `[[`, character(1L), "name") else character()
  if (anyDuplicated(provided_names)) stop("duplicate target-provided package", call. = FALSE)
  if (any(package_names %in% provided_names)) stop("package cannot be both synthetic and target-provided", call. = FALSE)

  provided_spec <- function(name) {
    index <- match(name, provided_names)
    if (is.na(index)) NULL else provided[[index]]
  }

  provided_libraries <- if (length(provided)) {
    vapply(provided, function(x) x$library, character(1L))
  } else {
    character()
  }
  .libPaths(.hrm_dedupe_libraries(c(provided_libraries, .Library)))

  verify_namespace <- function(name, ns) {
    if (identical(name, "base")) return(invisible(ns))
    expected <- provided_spec(name)
    if (is.null(expected)) {
      stop(sprintf("namespace '%s' was loaded outside the exact target-provided set", name), call. = FALSE)
    }
    actual_version <- as.character(getNamespaceVersion(ns))
    if (!identical(actual_version, expected$version)) {
      stop(sprintf("target package %s version mismatch: expected %s, loaded %s", name, expected$version, actual_version), call. = FALSE)
    }
    actual_path <- normalizePath(getNamespaceInfo(ns, "path"), winslash = "/", mustWork = TRUE)
    actual_library <- dirname(actual_path)
    expected_library <- .hrm_normalize_library(expected$library)
    if (!identical(.hrm_path_key(actual_library), .hrm_path_key(expected_library))) {
      stop(sprintf("target package %s library mismatch: expected %s, loaded %s", name, expected_library, actual_library), call. = FALSE)
    }
    invisible(ns)
  }

  verify_new_namespaces <- function(before) {
    for (name in setdiff(loadedNamespaces(), before)) {
      if (!identical(name, "base")) verify_namespace(name, getNamespace(name))
    }
  }

  for (pkg in provided) {
    path <- find.package(pkg$name, lib.loc = pkg$library, quiet = TRUE)
    if (!length(path)) stop(sprintf("target-provided package %s is absent from %s", pkg$name, pkg$library), call. = FALSE)
    description <- utils::read.dcf(file.path(path, "DESCRIPTION"), fields = c("Package", "Version"))
    if (!identical(unname(description[1L, "Version"]), pkg$version)) {
      stop(sprintf("target-provided package %s does not have required version %s", pkg$name, pkg$version), call. = FALSE)
    }
  }

  hermetic <- new.env(hash = TRUE, parent = emptyenv())
  synthetic_ns <- list()
  synthetic_imports <- list()
  synthetic_data <- list()

  for (pkg in packages) {
    imports <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
    ns <- new.env(hash = TRUE, parent = imports)
    assign(".packageName", pkg$name, envir = ns)
    assign(".__S3MethodsTable__.", new.env(hash = TRUE, parent = baseenv()), envir = ns)
    data_env <- new.env(hash = TRUE, parent = emptyenv())
    synthetic_imports[[pkg$name]] <- imports
    synthetic_ns[[pkg$name]] <- ns
    synthetic_data[[pkg$name]] <- data_env
    assign(paste0(pkg$name, "_ns"), ns, envir = hermetic)
    assign(paste0(pkg$name, "_data"), data_env, envir = hermetic)
  }

  resolve_namespace <- function(name) {
    if (name %in% package_names) return(synthetic_ns[[name]])
    if (identical(name, "base")) return(.BaseNamespaceEnv)
    expected <- provided_spec(name)
    if (is.null(expected)) {
      stop(sprintf("closure/import refers to non-target-provided namespace '%s'", name), call. = FALSE)
    }
    before <- loadedNamespaces()
    ns <- loadNamespace(name, lib.loc = expected$library)
    verify_namespace(name, ns)
    verify_new_namespaces(before)
    ns
  }

  for (pkg in packages) {
    recipes <- readRDS(pkg$recipes)
    missing <- setdiff(pkg$retained, names(recipes$bindings))
    if (length(missing)) {
      stop(sprintf("%s recipe is missing retained bindings: %s", pkg$name, paste(missing, collapse = ", ")), call. = FALSE)
    }
    ns <- synthetic_ns[[pkg$name]]
    for (name in pkg$retained) {
      assign(name, .hrm_rehydrate_recipe(recipes$bindings[[name]], resolve_namespace), envir = ns)
    }

    missing_data <- setdiff(pkg$datasets, names(recipes$datasets))
    if (length(missing_data)) {
      stop(sprintf("%s recipe is missing retained datasets: %s", pkg$name, paste(missing_data, collapse = ", ")), call. = FALSE)
    }
    data_env <- synthetic_data[[pkg$name]]
    for (name in pkg$datasets) {
      assign(name, .hrm_rehydrate_recipe(recipes$datasets[[name]], resolve_namespace), envir = data_env)
    }
  }

  package_spec <- function(name) {
    index <- match(name, package_names)
    if (is.na(index)) NULL else packages[[index]]
  }

  synthetic_public_names <- function(name) {
    pkg <- package_spec(name)
    if (is.null(pkg)) stop(sprintf("'%s' is not synthetic", name), call. = FALSE)
    vapply(pkg$exports, `[[`, character(1L), "name")
  }

  fetch_synthetic_public <- function(package, name) {
    pkg <- package_spec(package)
    public <- synthetic_public_names(package)
    index <- match(name, public)
    if (is.na(index)) stop(sprintf("synthetic package %s does not export '%s'", package, name), call. = FALSE)
    binding <- pkg$exports[[index]]$binding
    ns <- synthetic_ns[[package]]
    if (exists(binding, envir = ns, inherits = FALSE)) return(get(binding, envir = ns, inherits = FALSE))
    data_env <- synthetic_data[[package]]
    if (exists(binding, envir = data_env, inherits = FALSE)) return(get(binding, envir = data_env, inherits = FALSE))
    stop(sprintf("retained synthetic package %s has no backing binding '%s' for export '%s'", package, binding, name), call. = FALSE)
  }

  for (pkg in packages) {
    imports_env <- synthetic_imports[[pkg$name]]
    for (directive in pkg$imports) {
      from <- directive$package
      if (!(from %in% package_names)) {
        ns <- resolve_namespace(from)
        if (identical(directive$kind, "all")) {
          namespaceImport(imports_env, ns, from = pkg$name, except = directive$except)
        } else if (identical(directive$kind, "from")) {
          vars <- vapply(directive$bindings, `[[`, character(1L), "binding")
          names(vars) <- vapply(directive$bindings, `[[`, character(1L), "name")
          namespaceImportFrom(imports_env, ns, vars, from = pkg$name)
        } else {
          stop(sprintf("unknown import kind '%s'", directive$kind), call. = FALSE)
        }
        next
      }

      if (identical(directive$kind, "all")) {
        names <- setdiff(synthetic_public_names(from), directive$except)
        for (name in names) assign(name, fetch_synthetic_public(from, name), envir = imports_env)
      } else if (identical(directive$kind, "from")) {
        for (binding in directive$bindings) {
          assign(binding$name, fetch_synthetic_public(from, binding$binding), envir = imports_env)
        }
      } else {
        stop(sprintf("unknown import kind '%s'", directive$kind), call. = FALSE)
      }
    }
  }

  for (pkg in packages) {
    lockEnvironment(synthetic_imports[[pkg$name]], bindings = TRUE)
    lockEnvironment(synthetic_ns[[pkg$name]], bindings = TRUE)
    lockEnvironment(synthetic_data[[pkg$name]], bindings = TRUE)
  }
  lockEnvironment(hermetic, bindings = TRUE)

  saveRDS(hermetic, spec$output, version = 3L)
  if (!is.null(spec$lazy_db)) {
    db_env <- new.env(hash = TRUE, parent = emptyenv())
    assign(".hermetic", hermetic, envir = db_env)
    tools:::makeLazyLoadDB(db_env, spec$lazy_db, compress = TRUE)
  }

  invisible(NULL)
}
