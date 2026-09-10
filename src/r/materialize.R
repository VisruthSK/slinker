args <- commandArgs(trailingOnly = TRUE)
if (length(args) != 1L) stop("usage: materialize.R SPEC.R", call. = FALSE)
spec_env <- new.env(parent = baseenv())
sys.source(args[[1L]], envir = spec_env)
spec <- spec_env$spec
if (is.null(spec)) stop("materialization spec did not define 'spec'", call. = FALSE)

packages <- spec$packages
package_names <- vapply(packages, `[[`, character(1L), "name")
if (anyDuplicated(package_names)) stop("duplicate synthetic package", call. = FALSE)

provided <- spec$provided
provided_names <- if (length(provided)) vapply(provided, `[[`, character(1L), "name") else character()
if (anyDuplicated(provided_names)) stop("duplicate target-provided package", call. = FALSE)
if (any(package_names %in% provided_names)) stop("package cannot be both synthetic and target-provided", call. = FALSE)

normalize_library <- function(path) normalizePath(path, winslash = "/", mustWork = TRUE)
path_key <- function(path) {
  normalized <- normalize_library(path)
  if (.Platform$OS.type == "windows") tolower(normalized) else normalized
}
dedupe_libraries <- function(paths) {
  normalized <- vapply(paths, normalize_library, character(1L))
  normalized[!duplicated(vapply(normalized, path_key, character(1L)))]
}
provided_spec <- function(name) {
  index <- match(name, provided_names)
  if (is.na(index)) NULL else provided[[index]]
}

# Restrict dependency resolution to the declared target libraries plus R's base
# library. `loadNamespace()` may recursively load imports, so checking only the
# namespace requested by heRmetic is insufficient.
provided_libraries <- dedupe_libraries(vapply(provided, function(x) x$library, character(1L)))
.libPaths(dedupe_libraries(c(provided_libraries, .Library)))

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
  expected_library <- normalize_library(expected$library)
  if (!identical(path_key(actual_library), path_key(expected_library))) {
    stop(sprintf("target package %s library mismatch: expected %s, loaded %s", name, expected_library, actual_library), call. = FALSE)
  }
  invisible(ns)
}

verify_new_namespaces <- function(before) {
  new <- setdiff(loadedNamespaces(), before)
  for (name in new) {
    if (identical(name, "base")) next
    verify_namespace(name, getNamespace(name))
  }
}

# Fail before constructing anything if an exact target identity is absent.
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

missing_arg_box <- as.list(alist(.hrm_missing = ))
names(missing_arg_box) <- NULL

is_missing_slot <- function(values, i) {
  slot <- values[i]
  names(slot) <- NULL
  identical(slot, missing_arg_box)
}

rehydrate_slots <- function(x) {
  values <- as.list(x)
  out <- vector("list", length(values))
  if (length(values)) {
    for (i in seq_along(values)) {
      if (is_missing_slot(values, i)) {
        # Preserve R_MissingArg without turning it into a missing call argument.
        out[i] <- values[i]
      } else {
        out[i] <- list(rehydrate(values[[i]]))
      }
    }
  }
  names(out) <- names(values)
  out
}

rehydrate <- function(x) {
  type <- typeof(x)
  marker <- ".__hrm_recipe_env__."

  if (type == "closure") {
    ref <- attr(x, marker, exact = TRUE)
    if (is.null(ref)) stop("closure recipe has no environment reference", call. = FALSE)
    attrs <- attributes(x)
    attrs[[marker]] <- NULL
    if (length(attrs)) {
      for (name in names(attrs)) attrs[name] <- list(rehydrate(attrs[[name]]))
      attributes(x) <- attrs
    } else {
      attributes(x) <- NULL
    }
    if (startsWith(ref, "namespace:")) {
      environment(x) <- resolve_namespace(substring(ref, 11L))
    } else if (identical(ref, "base:base")) {
      environment(x) <- baseenv()
    } else {
      stop(sprintf("unknown closure environment recipe '%s'", ref), call. = FALSE)
    }
    return(x)
  }

  if (type == "list") {
    out <- rehydrate_slots(x)
  } else if (type == "expression") {
    out <- as.expression(rehydrate_slots(x))
  } else if (type == "pairlist") {
    out <- as.pairlist(rehydrate_slots(x))
  } else if (type == "language") {
    out <- as.call(rehydrate_slots(x))
  } else {
    out <- x
  }

  attrs <- attributes(x)
  if (length(attrs)) {
    for (name in names(attrs)) attrs[name] <- list(rehydrate(attrs[[name]]))
    attributes(out) <- attrs
  }
  out
}

for (pkg in packages) {
  recipes <- readRDS(pkg$recipes)
  available <- names(recipes$bindings)
  missing <- setdiff(pkg$retained, available)
  if (length(missing)) {
    stop(sprintf("%s recipe is missing retained bindings: %s", pkg$name, paste(missing, collapse = ", ")), call. = FALSE)
  }
  ns <- synthetic_ns[[pkg$name]]
  for (name in pkg$retained) assign(name, rehydrate(recipes$bindings[[name]]), envir = ns)

  missing_data <- setdiff(pkg$datasets, names(recipes$datasets))
  if (length(missing_data)) {
    stop(sprintf("%s recipe is missing retained datasets: %s", pkg$name, paste(missing_data, collapse = ", ")), call. = FALSE)
  }
  data_env <- synthetic_data[[pkg$name]]
  for (name in pkg$datasets) assign(name, rehydrate(recipes$datasets[[name]]), envir = data_env)
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
  if (exists(binding, envir = ns, inherits = FALSE)) {
    return(get(binding, envir = ns, inherits = FALSE))
  }
  data_env <- synthetic_data[[package]]
  if (exists(binding, envir = data_env, inherits = FALSE)) {
    return(get(binding, envir = data_env, inherits = FALSE))
  }
  stop(sprintf("retained synthetic package %s has no backing binding '%s' for export '%s'", package, binding, name), call. = FALSE)
}

for (pkg in packages) {
  imports_env <- synthetic_imports[[pkg$name]]
  for (directive in pkg$imports) {
    from <- directive$package
    if (!(from %in% package_names)) {
      # Delegate real-namespace binding transfer to R itself. This preserves
      # promises and active bindings instead of forcing and copying values.
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
