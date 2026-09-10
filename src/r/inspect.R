args <- commandArgs(trailingOnly = TRUE)
if (length(args) < 5L) {
  stop("usage: inspect.R LIBRARY PACKAGE OUTPUT RECIPES ANALYSIS_DIR [VISIBLE_LIBRARY ...]", call. = FALSE)
}

library <- normalizePath(args[[1L]], winslash = "/", mustWork = TRUE)
package <- args[[2L]]
output <- args[[3L]]
recipes_output <- args[[4L]]
analysis_dir <- args[[5L]]
visible_libraries <- if (length(args) > 5L) args[6L:length(args)] else library
normalize_library <- function(path) normalizePath(path, winslash = "/", mustWork = TRUE)
path_key <- function(path) {
  normalized <- normalizePath(path, winslash = "/", mustWork = FALSE)
  if (.Platform$OS.type == "windows") tolower(normalized) else normalized
}
visible_libraries <- vapply(c(library, visible_libraries, .Library), normalize_library, character(1L))
visible_libraries <- visible_libraries[!duplicated(vapply(visible_libraries, path_key, character(1L)))]
.libPaths(visible_libraries)

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

if (file.exists(output)) invisible(file.remove(output))
if (dir.exists(analysis_dir)) unlink(analysis_dir, recursive = TRUE, force = TRUE)
dir.create(file.path(analysis_dir, "R"), recursive = TRUE, showWarnings = FALSE)
invisible(file.create(file.path(analysis_dir, ".hrm-installed-image")))

pkgpath <- file.path(library, package)
if (!dir.exists(pkgpath)) stop(sprintf("installed package not found: %s", pkgpath), call. = FALSE)
pkgpath <- normalizePath(pkgpath, winslash = "/", mustWork = TRUE)

description_path <- file.path(pkgpath, "DESCRIPTION")
namespace_path <- file.path(pkgpath, "NAMESPACE")
ns_info_path <- file.path(pkgpath, "Meta", "nsInfo.rds")
package_rds_path <- file.path(pkgpath, "Meta", "package.rds")
for (path in c(description_path, ns_info_path, package_rds_path)) {
  if (!file.exists(path)) stop(sprintf("installed package metadata missing: %s", path), call. = FALSE)
}

invisible(file.copy(description_path, file.path(analysis_dir, "DESCRIPTION"), overwrite = TRUE))
if (file.exists(namespace_path)) {
  invisible(file.copy(namespace_path, file.path(analysis_dir, "NAMESPACE"), overwrite = TRUE))
} else {
  invisible(file.create(file.path(analysis_dir, "NAMESPACE")))
}

ns_info <- readRDS(ns_info_path)
pkg_info <- readRDS(package_rds_path)
version <- unname(pkg_info$DESCRIPTION[["Version"]])

# Load the installed lazy-load image directly into a private environment. This
# does not directly call loadNamespace() for this package, attach the package,
# run its .onLoad, register its S3 methods, or load its DLLs. The installed
# image is the semantic input heRmetic links from.
image_env <- new.env(hash = TRUE, parent = .BaseNamespaceEnv)
code_db <- file.path(pkgpath, "R", package)
if (!file.exists(paste0(code_db, ".rdx")) || !file.exists(paste0(code_db, ".rdb"))) {
  stop(sprintf("installed R lazy-load database missing for %s", package), call. = FALSE)
}
base::lazyLoad(code_db, envir = image_env)
code_names <- ls(image_env, all.names = TRUE)

sysdata_base <- file.path(pkgpath, "R", "sysdata")
if (file.exists(paste0(sysdata_base, ".rdx")) && file.exists(paste0(sysdata_base, ".rdb"))) {
  before <- ls(image_env, all.names = TRUE)
  base::lazyLoad(sysdata_base, envir = image_env)
  after <- ls(image_env, all.names = TRUE)
  sysdata_names <- setdiff(after, before)
} else {
  sysdata_names <- character()
}
all_names <- ls(image_env, all.names = TRUE)

missing_arg_box <- as.list(alist(.hrm_missing = ))
names(missing_arg_box) <- NULL

is_missing_slot <- function(values, i) {
  slot <- values[i]
  names(slot) <- NULL
  identical(slot, missing_arg_box)
}

scrub_slots <- function(x) {
  values <- as.list(x)
  out <- vector("list", length(values))
  if (length(values)) {
    for (i in seq_along(values)) {
      if (is_missing_slot(values, i)) {
        # Preserve R_MissingArg without passing it as an R function argument.
        out[i] <- values[i]
      } else {
        out[i] <- list(scrub_recipe(values[[i]]))
      }
    }
  }
  names(out) <- names(values)
  out
}

closure_environment <- function(env, path) {
  if (identical(env, image_env)) {
    return(list(issue = NULL, env = env_record(path, "namespace", package)))
  }
  if (isNamespace(env)) {
    return(list(issue = NULL, env = env_record(path, "namespace", getNamespaceName(env))))
  }
  if (identical(env, baseenv())) {
    return(list(issue = NULL, env = env_record(path, "base", "base")))
  }
  if (identical(env, .GlobalEnv)) {
    return(list(issue = issue(path, "environment_identity", ".GlobalEnv"), env = env_record(path, "global", ".GlobalEnv")))
  }
  name <- environmentName(env)
  list(
    issue = issue(path, "environment_identity", if (nzchar(name)) name else "local environment"),
    env = env_record(path, "local", name)
  )
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
        # Required function formals and omitted call arguments are represented
        # by R_MissingArg. Passing that sentinel to inspect_object() would make
        # its argument appear missing, so inspect only non-missing slots.
        if (!is_missing_slot(values, i)) {
          merge_child(inspect_object(values[[i]], paste0(path, "[[", i, "]]"), depth + 1L))
        }
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
  if (identical(env, image_env)) return(paste0("namespace:", package))
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
      for (name in names(attrs)) attrs[name] <- list(scrub_recipe(attrs[[name]]))
      attributes(out) <- attrs
    }
    attr(out, ".__hrm_recipe_env__.") <- ref
    return(out)
  }

  if (type == "list") {
    out <- scrub_slots(x)
  } else if (type == "expression") {
    out <- as.expression(scrub_slots(x))
  } else if (type == "pairlist") {
    out <- as.pairlist(scrub_slots(x))
  } else if (type == "language") {
    out <- as.call(scrub_slots(x))
  } else {
    out <- x
  }

  attrs <- attributes(x)
  if (length(attrs)) {
    for (name in names(attrs)) attrs[name] <- list(scrub_recipe(attrs[[name]]))
    attributes(out) <- attrs
  }
  out
}

quote_binding <- function(name) {
  if (grepl("^[A-Za-z.][A-Za-z0-9._]*$", name) && !grepl("^\\.[0-9]", name)) return(name)
  paste0("`", gsub("`", "\\\\`", name, fixed = TRUE), "`")
}

write_analysis_binding <- function(index, name, value) {
  lhs <- quote_binding(name)
  rhs <- if (typeof(value) == "closure") {
    paste(deparse(value, width.cutoff = 500L, control = c("keepInteger", "keepNA", "niceNames")), collapse = "\n")
  } else {
    # Non-closure installed objects are already materialized values. For
    # reachability they need identity, not reconstruction code.
    "NULL"
  }
  path <- file.path(analysis_dir, "R", sprintf("%06d.R", index))
  writeLines(paste0(lhs, " <- ", rhs), path, useBytes = TRUE)
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

recipes <- list(bindings = list(), datasets = list())
has_on_load <- ".onLoad" %in% all_names
emit("HEADER", package, version, if (has_on_load) "1" else "0")

if (length(ns_info$importClasses)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_import_classes", "importClasses is outside the v0.1 object model")
if (length(ns_info$importMethods)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_import_methods", "importMethods is outside the v0.1 object model")
if (length(ns_info$exportClasses)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_export_classes", "exportClasses is outside the v0.1 object model")
if (length(ns_info$exportMethods)) emit("PACKAGE_ISSUE", "NAMESPACE", "s4_export_methods", "exportMethods is outside the v0.1 object model")

exports <- as.character(ns_info$exports)
export_names <- names(ns_info$exports)
if (is.null(export_names)) export_names <- exports
empty_export_names <- !nzchar(export_names)
export_names[empty_export_names] <- exports[empty_export_names]
export_pairs <- Map(function(name, binding) c(name = name, binding = binding), export_names, exports)
for (pattern in ns_info$exportPatterns) {
  matches <- ls(image_env, pattern = pattern, all.names = TRUE)
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

for (i in seq_along(all_names)) {
  name <- all_names[[i]]
  value <- tryCatch(get(name, envir = image_env, inherits = FALSE), error = identity)
  origin <- if (name %in% sysdata_names) "sysdata" else "code"
  if (inherits(value, "error")) {
    emit("BINDING", name, origin, "unavailable", "0")
    emit("ISSUE", "$", "force_error", conditionMessage(value))
    next
  }
  write_analysis_binding(i, name, value)
  recipe <- emit_object("BINDING", name, value, origin)
  if (!is.null(recipe)) recipes$bindings[[name]] <- recipe
}

data_env <- new.env(hash = TRUE, parent = emptyenv())
data_base <- file.path(pkgpath, "data", "Rdata")
if (file.exists(paste0(data_base, ".rdx")) && file.exists(paste0(data_base, ".rdb"))) {
  base::lazyLoad(data_base, envir = data_env)
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
for (rel in sort(all_files)) emit("RESOURCE", chartr("\\", "/", rel))

saveRDS(recipes, recipes_output, version = 3L)
