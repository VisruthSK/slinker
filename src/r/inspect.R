args <- commandArgs(trailingOnly = TRUE)
if (length(args) < 5L) stop("usage: inspect.R LIBRARY PACKAGE OUTPUT RECIPES NATIVE_PROBE [VISIBLE_LIBRARY ...]", call. = FALSE)
library <- normalizePath(args[[1L]], winslash = "/", mustWork = TRUE)
package <- args[[2L]]
output <- args[[3L]]
recipes_output <- args[[4L]]
native_probe <- normalizePath(args[[5L]], winslash = "/", mustWork = TRUE)
visible_libraries <- if (length(args) > 5L) args[6L:length(args)] else library
normalize_library <- function(path) normalizePath(path, winslash = "/", mustWork = TRUE)
path_key <- function(path) {
  normalized <- normalizePath(path, winslash = "/", mustWork = FALSE)
  if (.Platform$OS.type == "windows") tolower(normalized) else normalized
}
dedupe_paths <- function(paths) {
  normalized <- vapply(paths, normalize_library, character(1L))
  normalized[!duplicated(vapply(normalized, path_key, character(1L)))]
}
visible_libraries <- dedupe_paths(c(library, visible_libraries))
.libPaths(dedupe_paths(c(visible_libraries, .Library)))
native_probe_dll <- dyn.load(native_probe, local = TRUE, now = TRUE)
native_probe_altrep <- getNativeSymbolInfo("hrm_altrep_info", PACKAGE = native_probe_dll)$address

hex <- function(x) {
  x <- enc2utf8(as.character(x))
  paste(sprintf("%02x", as.integer(charToRaw(x))), collapse = "")
}

emit <- function(kind, ...) {
  fields <- vapply(list(...), hex, character(1L), USE.NAMES = FALSE)
  cat(paste(c(kind, fields), collapse = "\t"), "\n", file = output, append = TRUE, sep = "")
}

issue <- function(path, kind, detail = "") list(path = path, kind = kind, detail = detail)
env_record <- function(path, kind, name = "") list(path = path, kind = kind, name = name)

closure_environment <- function(env, path) {
  if (isNamespace(env)) {
    return(list(issue = NULL, env = env_record(path, "namespace", getNamespaceName(env))))
  }
  if (identical(env, .GlobalEnv)) {
    return(list(issue = issue(path, "environment_identity", ".GlobalEnv"), env = env_record(path, "global", ".GlobalEnv")))
  }
  if (identical(env, baseenv())) {
    return(list(issue = NULL, env = env_record(path, "base", "base")))
  }
  if (identical(env, emptyenv())) {
    return(list(issue = issue(path, "environment_identity", "emptyenv"), env = env_record(path, "empty", "")))
  }
  name <- environmentName(env)
  list(issue = issue(path, "environment_identity", if (nzchar(name)) name else "local environment"), env = env_record(path, "local", name))
}

inspect_object <- function(x, path = "$", depth = 0L) {
  issues <- list()
  envs <- list()
  add_issue <- function(value) issues[[length(issues) + 1L]] <<- value
  add_env <- function(value) envs[[length(envs) + 1L]] <<- value
  merge_child <- function(child) {
    if (length(child$issues)) issues <<- c(issues, child$issues)
    if (length(child$envs)) envs <<- c(envs, child$envs)
  }

  if (depth > 128L) {
    add_issue(issue(path, "object_depth", "object graph exceeds 128 levels"))
    return(list(type = typeof(x), issues = issues, envs = envs))
  }

  type <- typeof(x)
  altrep <- .Call(native_probe_altrep, x)
  if (identical(altrep[[1L]], "1") && !identical(altrep[[3L]], "base")) {
    add_issue(issue(path, "unsupported_altrep", paste0(altrep[[3L]], "::", altrep[[2L]])))
  }
  if (isS4(x)) add_issue(issue(path, "s4", paste(class(x), collapse = "/")))
  classes <- class(x)
  if (length(classes) && any(grepl("^S7", classes))) {
    add_issue(issue(path, "s7", paste(classes, collapse = "/")))
  }

  if (type == "environment") {
    add_issue(issue(path, "environment", "embedded environment"))
  } else if (type == "externalptr") {
    add_issue(issue(path, "external_pointer", "external pointer"))
  } else if (type == "weakref") {
    add_issue(issue(path, "weak_reference", "weak reference"))
  } else if (type == "closure") {
    env_result <- closure_environment(environment(x), paste0(path, ".environment"))
    add_env(env_result$env)
    if (!is.null(env_result$issue)) add_issue(env_result$issue)
    merge_child(inspect_object(formals(x), paste0(path, ".formals"), depth + 1L))
    merge_child(inspect_object(body(x), paste0(path, ".body"), depth + 1L))
  } else if (type %in% c("list", "expression", "pairlist", "language")) {
    values <- as.list(x)
    if (length(values)) {
      for (i in seq_along(values)) {
        merge_child(inspect_object(values[[i]], paste0(path, "[[", i, "]]"), depth + 1L))
      }
    }
  } else if (!(type %in% c("NULL", "logical", "integer", "double", "complex", "character", "raw", "symbol", "builtin", "special"))) {
    add_issue(issue(path, "unsupported_type", type))
  }

  attrs <- attributes(x)
  if (length(attrs)) {
    for (name in names(attrs)) {
      merge_child(inspect_object(attrs[[name]], paste0(path, ".attr[", name, "]"), depth + 1L))
    }
  }

  list(type = type, issues = issues, envs = envs)
}


recipe_env_ref <- function(env) {
  if (isNamespace(env)) return(paste0("namespace:", getNamespaceName(env)))
  if (identical(env, baseenv())) return("base:base")
  stop("unsupported closure environment reached recipe generation", call. = FALSE)
}

scrub_recipe <- function(x) {
  type <- typeof(x)
  if (type == "closure") {
    out <- x
    ref <- recipe_env_ref(environment(out))
    environment(out) <- emptyenv()
    attrs <- attributes(out)
    if (length(attrs)) {
      for (name in names(attrs)) attrs[[name]] <- scrub_recipe(attrs[[name]])
      attributes(out) <- attrs
    }
    attr(out, ".__hrm_recipe_env__.") <- ref
    return(out)
  }

  if (type == "list") {
    out <- vector("list", length(x))
    if (length(x)) for (i in seq_along(x)) out[[i]] <- scrub_recipe(x[[i]])
    names(out) <- names(x)
  } else if (type == "expression") {
    out <- as.expression(lapply(as.list(x), scrub_recipe))
  } else if (type == "pairlist") {
    out <- as.pairlist(lapply(as.list(x), scrub_recipe))
    names(out) <- names(x)
  } else if (type == "language") {
    out <- as.call(lapply(as.list(x), scrub_recipe))
  } else {
    out <- x
  }

  attrs <- attributes(x)
  if (length(attrs)) {
    for (name in names(attrs)) attrs[[name]] <- scrub_recipe(attrs[[name]])
    attributes(out) <- attrs
  }
  out
}

emit_object <- function(kind, name, value, origin = NULL) {
  result <- inspect_object(value)
  supported <- if (length(result$issues)) "0" else "1"
  if (kind == "BINDING") emit(kind, name, origin, result$type, supported)
  else emit(kind, name, result$type, supported)
  for (x in result$issues) emit("ISSUE", x$path, x$kind, x$detail)
  for (x in result$envs) emit("CLOSURE_ENV", x$path, x$kind, x$name)
  if (supported == "1") return(scrub_recipe(value))
  NULL
}

if (file.exists(output)) file.remove(output)
recipes <- list(bindings = list(), datasets = list())
pkgpath <- find.package(package, lib.loc = library, quiet = TRUE)
if (!length(pkgpath)) stop("package not found in staged library", call. = FALSE)
pkgpath <- normalizePath(pkgpath, winslash = "/", mustWork = TRUE)

if (package %in% loadedNamespaces()) stop("package already loaded before inspection", call. = FALSE)
before_dll <- vapply(getLoadedDLLs(), function(x) x[["path"]], character(1L), USE.NAMES = FALSE)

ns_info_path <- file.path(pkgpath, "Meta", "nsInfo.rds")
ns_info <- if (file.exists(ns_info_path)) readRDS(ns_info_path) else parseNamespaceFile(package, dirname(pkgpath), mustExist = FALSE)
pkg_info <- readRDS(file.path(pkgpath, "Meta", "package.rds"))
version <- unname(pkg_info$DESCRIPTION[["Version"]])

ns <- loadNamespace(package, lib.loc = library, partial = TRUE)
metadata_names <- c(".__NAMESPACE__.", ".__S3MethodsTable__.", ".packageName")
code_names <- setdiff(ls(ns, all.names = TRUE), metadata_names)

sysdata_base <- file.path(pkgpath, "R", "sysdata")
if (file.exists(paste0(sysdata_base, ".rdb"))) lazyLoad(sysdata_base, ns)
after_sysdata_names <- setdiff(ls(ns, all.names = TRUE), metadata_names)
sysdata_names <- setdiff(after_sysdata_names, code_names)

data_env <- new.env(hash = TRUE, parent = emptyenv())
data_base <- file.path(pkgpath, "data", "Rdata")
if (file.exists(paste0(data_base, ".rdb"))) lazyLoad(data_base, data_env)

after_dll <- vapply(getLoadedDLLs(), function(x) x[["path"]], character(1L), USE.NAMES = FALSE)
new_dll <- setdiff(after_dll, before_dll)
normalized_pkgpath <- normalizePath(pkgpath, winslash = "/", mustWork = TRUE)
pkgpath_key <- path_key(normalized_pkgpath)
new_dll_keys <- if (length(new_dll)) vapply(new_dll, path_key, character(1L)) else character()
own_dll <- any(startsWith(new_dll_keys, paste0(pkgpath_key, "/")))
activation_clean <- !own_dll && !environmentIsLocked(ns)
has_on_load <- exists(".onLoad", envir = ns, inherits = FALSE)
emit("HEADER", package, version, if (activation_clean) "1" else "0", if (has_on_load) "1" else "0")

if (length(ns_info$importClasses)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_import_classes", "importClasses is outside the MVP object model")
if (length(ns_info$importMethods)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_import_methods", "importMethods is outside the MVP object model")
if (length(ns_info$exportClasses)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_export_classes", "exportClasses is outside the MVP object model")
if (length(ns_info$exportMethods)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_export_methods", "exportMethods is outside the MVP object model")

exports <- as.character(ns_info$exports)
export_names <- names(ns_info$exports)
if (is.null(export_names)) export_names <- exports
empty_export_names <- !nzchar(export_names)
export_names[empty_export_names] <- exports[empty_export_names]
export_pairs <- Map(function(name, binding) c(name = name, binding = binding), export_names, exports)
for (pattern in ns_info$exportPatterns) {
  matches <- ls(ns, pattern = pattern, all.names = TRUE)
  export_pairs <- c(export_pairs, Map(function(name) c(name = name, binding = name), matches))
}
if (length(export_pairs)) {
  keys <- vapply(export_pairs, function(x) paste0(x[["name"]], "\r", x[["binding"]]), character(1L))
  export_pairs <- export_pairs[!duplicated(keys)]
  export_pairs <- export_pairs[order(vapply(export_pairs, `[[`, character(1L), "name"))]
  for (pair in export_pairs) emit("EXPORT", pair[["name"]], pair[["binding"]])
}

for (entry in ns_info$imports) {
  if (is.character(entry)) {
    emit("IMPORT_ALL", entry)
  } else if (!is.null(entry$except)) {
    from <- as.character(entry[[1L]])
    emit("IMPORT_ALL", from)
    for (name in as.character(entry$except)) emit("IMPORT_EXCEPT", from, name)
  } else {
    from <- as.character(entry[[1L]])
    vars <- as.character(entry[[2L]])
    local_names <- names(entry[[2L]])
    if (is.null(local_names)) local_names <- vars
    empty_names <- !nzchar(local_names)
    local_names[empty_names] <- vars[empty_names]
    for (i in seq_along(vars)) emit("IMPORT_FROM", from, vars[[i]], local_names[[i]])
  }
}

for (name in sort(after_sysdata_names)) {
  origin <- if (name %in% sysdata_names) "sysdata" else "code"
  if (bindingIsActive(name, ns)) {
    emit("BINDING", name, origin, "active_binding", "0")
    emit("ISSUE", "$", "active_binding", "active binding in persistent namespace state")
  } else {
    value <- tryCatch(get(name, envir = ns, inherits = FALSE), error = identity)
    if (inherits(value, "error")) {
      emit("BINDING", name, origin, "unavailable", "0")
      emit("ISSUE", "$", "force_error", conditionMessage(value))
    } else {
      recipe <- emit_object("BINDING", name, value, origin)
      if (!is.null(recipe)) recipes$bindings[[name]] <- recipe
    }
  }
}

for (name in sort(ls(data_env, all.names = TRUE))) {
  value <- tryCatch(get(name, envir = data_env, inherits = FALSE), error = identity)
  if (inherits(value, "error")) {
    emit("DATASET", name, "unavailable", "0")
    emit("ISSUE", "$", "force_error", conditionMessage(value))
  } else {
    recipe <- emit_object("DATASET", name, value)
    if (!is.null(recipe)) recipes$datasets[[name]] <- recipe
  }
}

s3 <- ns_info$S3methods
if (length(s3)) {
  s3 <- as.matrix(s3)
  for (i in seq_len(nrow(s3))) {
    method <- if (ncol(s3) >= 3L && !is.na(s3[i, 3L])) s3[i, 3L] else paste(s3[i, 1L], s3[i, 2L], sep = ".")
    emit("S3", s3[i, 1L], s3[i, 2L], method)
  }
}

for (dll in as.character(ns_info$dynlibs)) emit("DYNLIB", dll)

all_files <- list.files(pkgpath, recursive = TRUE, all.files = TRUE, full.names = FALSE, include.dirs = FALSE, no.. = TRUE)
for (rel in sort(all_files)) {
  # Protocol paths are package-relative and always use forward slashes,
  # independent of the target host's native separator.
  emit("RESOURCE", chartr("\\", "/", rel))
}

saveRDS(recipes, recipes_output, version = 3L)
